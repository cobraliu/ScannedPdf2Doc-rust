//! 块序列 -> docx
//!
//! 跨页续接的状态机跟 Python 版一致: 上一页结尾是表、这一页开头也是同一套列的
//! 表, 就往同一张表里加行, 而不是新起一张 —— 否则一份 96 页的合同能出 113 张
//! 支离破碎的表。

use crate::config::Config;
use crate::docx::{Docx, Fmt, CM};
use crate::layout::grid::Grid;
use crate::layout::para::{
    build_rows, bullet, end_punct, heading_level, is_header_row, mark_bullets, merge_paras,
    num_start,
};
use crate::layout::{Block, Line, Page};

/// 跨页续接用的记忆
#[derive(Default)]
pub struct State {
    tbl: Option<TblState>,
    grid: Option<GridState>,
    /// 还没落地的「原第 N 页」标记
    marker: Option<String>,
}

struct TblState {
    id: usize,
    starts: Vec<f32>,
    page: usize,
    header: bool,
    widths: Vec<i32>,
}

struct GridState {
    id: usize,
    rxs: Vec<f32>,
    page: usize,
    ncols: usize,
    widths: Vec<i32>,
    /// 已经写进这张表的行数 —— vMerge 要按整表行号算
    rows: usize,
}

/// 列多了得缩字号
fn grid_size(nc: usize) -> f32 {
    if nc <= 6 {
        10.0
    } else if nc <= 10 {
        9.0
    } else {
        8.0
    }
}

/// 本页开头是不是上一页那张表的续表: 列数与列位置都对得上
fn continues(prev: &Option<TblState>, blocks: &[Block], page: usize) -> bool {
    let (Some(p), Some(Block::Table(_, starts))) = (prev, blocks.first()) else {
        return false;
    };
    p.page + 1 == page
        && p.starts.len() == starts.len()
        && p.starts
            .iter()
            .zip(starts)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max)
            < 0.05
}

fn grid_continues(prev: &Option<GridState>, blocks: &[Block], page: usize) -> bool {
    let (Some(p), Some(Block::Grid(g))) = (prev, blocks.first()) else {
        return false;
    };
    p.page + 1 == page
        && p.rxs.len() == g.rxs.len()
        && p.rxs
            .iter()
            .zip(&g.rxs)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max)
            < 0.02
}

/// 一页写进文档
pub fn render_page(doc: &mut Docx, page: &Page, page_no: usize, cfg: &Config, st: &mut State) {
    for (bi, blk) in page.blocks.iter().enumerate() {
        match blk {
            Block::Grid(g) => {
                if bi == 0 && grid_continues(&st.grid, &page.blocks, page_no) {
                    let gs = st.grid.as_mut().unwrap();
                    let (id, widths, ncols, r0) =
                        (gs.id, gs.widths.clone(), gs.ncols, gs.rows);
                    if let Some(label) = st.marker.take() {
                        let total: i32 = widths.iter().sum();
                        let row = doc.marker_row(&label, ncols, total);
                        doc.push_row(id, &row);
                    }
                    let rows = write_grid(doc, id, g, &widths, false);
                    let gs = st.grid.as_mut().unwrap();
                    gs.rows = r0 + rows;
                    gs.page = page_no;
                } else {
                    let widths = grid_widths(doc, g);
                    let id = doc.new_table(&widths, true);
                    let head = is_header_row(&g.rows());
                    let rows = write_grid(doc, id, g, &widths, head);
                    doc.blank();
                    st.grid = Some(GridState {
                        id,
                        rxs: g.rxs.clone(),
                        page: page_no,
                        ncols: g.ncols(),
                        widths,
                        rows,
                    });
                }
                st.tbl = None; // 框线表不跟无框线表互相续接
            }
            Block::Table(lines, starts) => {
                let rows = build_rows(lines, starts);
                if rows.is_empty() {
                    continue;
                }
                // 只有一行且首列为空 = 上页续行漂过来的孤立文字, 不是真表格
                if rows.len() == 1 && rows[0][0].is_empty() {
                    let t = rows[0]
                        .iter()
                        .filter(|c| !c.is_empty())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ");
                    doc.para(&t, &Fmt::new(cfg.font_size), 0, false, false);
                    continue;
                }
                if bi == 0 && continues(&st.tbl, &page.blocks, page_no) {
                    let ts = st.tbl.as_ref().unwrap();
                    let (id, widths, header) = (ts.id, ts.widths.clone(), ts.header);
                    if let Some(label) = st.marker.take() {
                        let total: i32 = widths.iter().sum();
                        let row = doc.marker_row(&label, widths.len(), total);
                        doc.push_row(id, &row);
                    }
                    let bold_first = !header && starts.len() == 2;
                    for txt in &rows {
                        let r = fill_row(doc, txt, &widths, bold_first, false);
                        doc.push_row(id, &r);
                    }
                    let ts = st.tbl.as_mut().unwrap();
                    ts.page = page_no;
                } else {
                    let header = is_header_row(&rows);
                    let widths = tbl_widths(doc, starts);
                    let id = doc.new_table(&widths, false);
                    let bold_first = !header && starts.len() == 2;
                    for (n, txt) in rows.iter().enumerate() {
                        let r = fill_row(doc, txt, &widths, bold_first, header && n == 0);
                        doc.push_row(id, &r);
                    }
                    doc.blank();
                    st.tbl = Some(TblState {
                        id,
                        starts: starts.clone(),
                        page: page_no,
                        header,
                        widths,
                    });
                }
                st.grid = None;
            }
            Block::Text(lines) => {
                st.grid = None; // 中间隔了正文就不是同一张框线表了
                // 注意不清 st.tbl: 无框线表的续表判据是"列数列位对得上", 中间隔了
                // 正文照样能接。合同里的条款表天天这样 —— 表格末尾几行说明文字之后
                // 翻页继续列条款, 清掉状态就把一张表切成两张了。
                write_text(doc, lines, cfg);
            }
        }
    }
}

fn tbl_widths(doc: &Docx, starts: &[f32]) -> Vec<i32> {
    let mut edges = starts.to_vec();
    edges.push(1.0);
    let span: Vec<f32> = (0..starts.len()).map(|i| edges[i + 1] - edges[i]).collect();
    let total: f32 = span.iter().sum::<f32>().max(1e-6);
    let w = doc.usable_w();
    span.iter()
        .map(|s| (w * s / total).max(1.2 * CM).round() as i32)
        .collect()
}

fn grid_widths(doc: &Docx, g: &Grid) -> Vec<i32> {
    let rxs = &g.rxs;
    let total = (rxs[rxs.len() - 1] - rxs[0]).max(1e-6);
    let w = doc.usable_w();
    (0..g.ncols())
        .map(|i| (w * (rxs[i + 1] - rxs[i]) / total).max(0.5 * CM).round() as i32)
        .collect()
}

fn fill_row(doc: &Docx, txt: &[String], widths: &[i32], bold_first: bool, header: bool) -> String {
    let mut cells = String::new();
    for (k, t) in txt.iter().enumerate() {
        let f = Fmt::new(10.0).bold(header || (bold_first && k == 0));
        cells.push_str(&doc.cell(t, widths[k], 1, None, &f, false));
    }
    doc.row(&cells, header)
}

/// 把网格写进表格, 返回写了多少行。原件合并的格子在这里也合并。
fn write_grid(doc: &mut Docx, id: usize, g: &Grid, widths: &[i32], head: bool) -> usize {
    let size = grid_size(g.ncols());
    let (nr, nc) = (g.nrows(), g.ncols());
    // (r,c) -> 该位置属于哪个 cell, 以及它是不是这个 cell 的左上角
    let mut owner: Vec<Vec<Option<usize>>> = vec![vec![None; nc]; nr];
    for (k, cell) in g.cells.iter().enumerate() {
        for i in cell.r..cell.r + cell.h {
            for j in cell.c..cell.c + cell.w {
                owner[i][j] = Some(k);
            }
        }
    }
    for r in 0..nr {
        let mut row = String::new();
        let mut c = 0usize;
        while c < nc {
            let Some(k) = owner[r][c] else {
                row.push_str(&doc.cell("", widths[c], 1, None, &Fmt::new(size), false));
                c += 1;
                continue;
            };
            let cell = &g.cells[k];
            let w: i32 = widths[cell.c..cell.c + cell.w].iter().sum();
            let vm = if cell.h > 1 {
                Some(r == cell.r) // 起始行 restart, 其余是被并掉的格
            } else {
                None
            };
            // 竖向合并的后续行不重复写文字
            let text = if vm == Some(false) { "" } else { cell.text.as_str() };
            let f = Fmt::new(size).bold(head && r == 0);
            let center = cell.w > 1 || cell.h > 1;
            row.push_str(&doc.cell(text, w, cell.w, vm, &f, center));
            c += cell.w;
        }
        let r_xml = doc.row(&row, head && r == 0);
        doc.push_row(id, &r_xml);
    }
    nr
}

fn write_text(doc: &mut Docx, lines: &[Line], cfg: &Config) {
    let paras = mark_bullets(merge_paras(lines, cfg), cfg);
    for i in 0..paras.len() {
        let p = &paras[i];
        let t = p.text.clone();
        if let Some(m) = bullet().find(&t) {
            let rest = t[m.end()..].to_string();
            doc.para(&rest, &Fmt::new(cfg.font_size), 0, false, true);
            continue;
        }
        if p.bullet {
            doc.para(&t, &Fmt::new(cfg.font_size), 0, false, true);
            continue;
        }
        if let Some(lv) = heading_level(&t) {
            doc.para(&t, &Fmt::new(12.0 - lv as f32).bold(true), 0, false, false);
            continue;
        }
        // 短行 + 无收尾标点 + 下一行是长正文 => 无编号小标题
        let nxt_long = paras.get(i + 1).map(|n| n.rx1 > cfg.full_line).unwrap_or(false);
        if p.rx1 < 0.58
            && !end_punct().is_match(&t)
            && t.chars().count() < 60
            && !num_start().is_match(&t)
            && nxt_long
        {
            doc.para(&t, &Fmt::new(cfg.font_size).bold(true), 0, false, false);
            continue;
        }
        if p.center {
            doc.para(&t, &Fmt::new(13.0).bold(true), 0, true, false);
            continue;
        }
        let lv = if p.cx0 > 0.16 { 1 } else { 0 };
        doc.para(&t, &Fmt::new(cfg.font_size), lv, false, false);
    }
}

/// 页与页之间插「原第 N 页」
///
/// 这一页开头如果要并进上一页那张表, 标记就不能是独立段落 —— 那会把表切断。
/// 交给续表逻辑, 做成表内一整行。
pub fn page_marker(doc: &mut Docx, page: &Page, page_no: usize, st: &mut State) {
    if continues(&st.tbl, &page.blocks, page_no) || grid_continues(&st.grid, &page.blocks, page_no)
    {
        st.marker = Some(page_no.to_string());
        return;
    }
    st.marker = None;
    doc.para(
        &format!("—— 原第 {page_no} 页 ——"),
        &Fmt::new(8.0).color("999999"),
        0,
        true,
        false,
    );
}

/// 单页失败: 留一条醒目占位, 其余页照常输出
pub fn page_failed(doc: &mut Docx, page_no: usize, err: &str, st: &mut State) {
    doc.para(
        &format!("[第 {page_no} 页解析失败, 已跳过: {err}]"),
        &Fmt::new(10.5).bold(true).color("C03030"),
        0,
        false,
        false,
    );
    *st = State::default(); // 断了就别再往上一页的表里接
}
