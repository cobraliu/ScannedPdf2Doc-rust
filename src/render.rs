//! 块序列 -> docx
//!
//! 跨页续接的状态机跟 Python 版一致: 上一页结尾是表、这一页开头也是同一套列的
//! 表, 就往同一张表里加行, 而不是新起一张 —— 否则一份 96 页的合同能出 113 张
//! 支离破碎的表。

use crate::config::Config;
use crate::docx::{Align, CellOpt, Docx, Fmt, CM};
use crate::layout::grid;
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
    /// 全文见过的缩进档位, 从左到右
    ind: Vec<(f32, usize)>,
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

/// 先把每一页的缩进过一遍, 全过完再开始排版
///
/// 必须走两趟。零点是"全文最靠左的那一档", 边排边攒的话, 排第 1 页时手上只有
/// 第 1 页的数据 —— 实测 配套.pdf 前四页算出来的零点是 0.0866, 第 5 页起变成
/// 0.0618, 同一个横坐标在第 4 页和第 5 页会落到不同的缩进级。那份文件里两段
/// 的物理页边距本来就不同, 撞对了; 换一份"最窄的页边距在后面才出现"的, 前面
/// 几页就会整篇凭空多缩一格。
pub fn scan_indents(st: &mut State, page: &Page, cfg: &Config) {
    merge_indents(&mut st.ind, page, cfg);
}

/// 一页写进文档 —— 调之前每一页都得先过 [`scan_indents`]
pub fn render_page(doc: &mut Docx, page: &Page, page_no: usize, cfg: &Config, st: &mut State) {
    let base = indent_base(&st.ind);
    for (bi, blk) in page.blocks.iter().enumerate() {
        match blk {
            Block::Grid(g) => {
                if bi == 0 && grid_continues(&st.grid, &page.blocks, page_no) {
                    let gs = st.grid.as_mut().unwrap();
                    let (id, widths, ncols, r0) = (gs.id, gs.widths.clone(), gs.ncols, gs.rows);
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
                    doc.para(&t, &Fmt::new(cfg.font_size), 0, Align::Left, false);
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
                write_text(doc, lines, cfg, base);
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
        cells.push_str(&doc.cell(t, widths[k], &f, &CellOpt::default()));
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
        for row in &mut owner[cell.r..cell.r + cell.h] {
            row[cell.c..cell.c + cell.w].fill(Some(k));
        }
    }
    for (r, orow) in owner.iter().enumerate() {
        let mut row = String::new();
        let mut c = 0usize;
        while c < nc {
            let Some(k) = orow[c] else {
                row.push_str(&doc.cell("", widths[c], &Fmt::new(size), &CellOpt::default()));
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
            let text = if vm == Some(false) {
                ""
            } else {
                cell.text.as_str()
            };
            let f = Fmt::new(size).bold(head && r == 0);
            // 照原件逐边画: 签章表只有外框没有内线、三线表只画上下两条,
            // 一律铺满反而不像原件。
            // 纵向合并的格子每一行都要写一遍, 但上边框只属于第一行、下边框只
            // 属于最后一行 —— 每行都写一遍会在合并块中间横生出几条线来。
            let mut edges = cell.edges;
            if r != cell.r {
                edges[grid::TOP] = false;
            }
            if r != cell.r + cell.h - 1 {
                edges[grid::BOTTOM] = false;
            }
            let o = CellOpt {
                span: cell.w,
                vmerge: vm,
                center: cell.w > 1 || cell.h > 1,
                edges: Some(edges),
            };
            row.push_str(&doc.cell(text, w, &f, &o));
            c += cell.w;
        }
        let r_xml = doc.row(&row, head && r == 0);
        doc.push_row(id, &r_xml);
    }
    nr
}

/// 一档要出现这么多次才够格当零点
const IND_MIN_HITS: usize = 3;

/// 把这一页的缩进并进全文的档位表
///
/// 表只用来定零点。缩几级是按"离零点多远"折算的, 不按这一档排第几:
/// 排第几只在整页都是大纲时才等于层级, 而合同报价单里 96 页攒下来是 24 档
/// (0.12 一直排到 0.88) —— 那是多栏表格的各列横坐标, 第 6 档并不比第 5 档
/// 深一级。按距离折算, 各列就落在它原本该在的位置上。
///
/// 档位得攒在 State 里跨页累积, 不能一页一算: 某一页未必出现最外层。
/// Conclusion-for-QA 第 2 页整页都是二级往下的条目(唯一一条一级的落在页脚被
/// 丢了), 单看这页, 二级就成了零点 —— 同一级的条目第 1 页缩一格、第 2 页顶格。
///
/// 档的代表值一旦定下就不再动, 免得一串缓慢右移的行首尾相接把两档连成一档。
///
/// 归档的容差取半级: 同一档的行受识别框抖动影响差个几千分之一, 得算一档;
/// 真差一级的差一整个 ind_step, 分得开。
fn merge_indents(lv: &mut Vec<(f32, usize)>, page: &Page, cfg: &Config) {
    let tol = cfg.ind_step / 2.0;
    let mut xs: Vec<f32> = page
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text(lines) => Some(lines.iter().map(|l| l.cx0)),
            _ => None,
        })
        .flatten()
        .collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    for x in xs {
        match lv.iter_mut().find(|(c, _)| (x - c).abs() <= tol) {
            Some((_, n)) => *n += 1,
            None => {
                let at = lv.partition_point(|(c, _)| *c < x);
                lv.insert(at, (x, 1));
            }
        }
    }
}

/// 缩进的零点: 最靠左的那一档, 但得站得住脚
///
/// 不直接取最小值: 96 页里只要有一行被识别框往左歪了一次, 整篇的零点就跟着
/// 左移, 后面每一段凭空多缩两三级。要求这一档至少出现过几次, 偶发的歪行就
/// 落选了。
///
/// 一份文件里两段的物理页边距不同时, 只能顾一头: 配套.pdf 前四页的正文在
/// 0.0867, 第五页起的附件在 0.0618, 零点取 0.0618, 前四页就整篇缩了一格。
/// 这是全文一个零点的代价 —— 换成一页一个零点, 页内的相对关系是对了, 页与
/// 页之间又对不上(见 [`scan_indents`]), 那个错得更难看。
fn indent_base(lv: &[(f32, usize)]) -> f32 {
    lv.iter()
        .find(|(_, n)| *n >= IND_MIN_HITS)
        .or_else(|| lv.first())
        .map(|(c, _)| *c)
        .unwrap_or(0.0)
}

/// 量出来该缩几级 —— 按离零点多远折算, 落回 Word 的一级 0.74cm。不封顶
fn indent_raw(cx0: f32, base: f32, cfg: &Config) -> i32 {
    ((cx0 - base) / cfg.ind_step).round() as i32
}

/// 缩几级, 封到 ind_max
fn indent_of(cx0: f32, base: f32, cfg: &Config) -> u8 {
    indent_raw(cx0, base, cfg).clamp(0, cfg.ind_max as i32) as u8
}

/// 这行是不是靠右摆的
///
/// 缩到 ind_max 还放不下, 说明它压根不在正文那条缩进阶梯上 —— 右上角页眉的
/// 页码、合同编号、落款都是这样。硬按缩进写, 顶到 11.1cm 只剩 4.9cm, 十来个
/// 字要折成两三行; 右对齐才是它在原件上的样子。
///
/// 还要求短: 长段落缩到那么右本来就写不下, 它是右栏正文被并串了, 不是靠右摆。
fn is_right_aligned(cx0: f32, rx1: f32, base: f32, cfg: &Config) -> bool {
    indent_raw(cx0, base, cfg) > cfg.ind_max as i32 && (rx1 - cx0) < 0.25
}

fn write_text(doc: &mut Docx, lines: &[Line], cfg: &Config, base: f32) {
    let paras = mark_bullets(merge_paras(lines, cfg), cfg);
    for i in 0..paras.len() {
        let p = &paras[i];
        let t = p.text.clone();
        // 原来缩进只有"有/无"两档(cx0 > 0.16), 表达不了多级嵌套:
        // "1. -> a. -> i. -> 1." 实测落在 0.046 / 0.079 / 0.107 / 0.142,
        // 四档全在 0.16 以下, 整页大纲于是一路拍平成一级, 看上去就是版式全丢。
        let ind = indent_of(p.cx0, base, cfg);
        // 项目符号也吃缩进 —— 嵌套清单里 • 一样分层, 全顶格就看不出谁属于谁
        if let Some(m) = bullet().find(&t) {
            let rest = t[m.end()..].to_string();
            doc.para(&rest, &Fmt::new(cfg.font_size), ind, Align::Left, true);
            continue;
        }
        if p.bullet {
            doc.para(&t, &Fmt::new(cfg.font_size), ind, Align::Left, true);
            continue;
        }
        if is_right_aligned(p.cx0, p.rx1, base, cfg) {
            doc.para(&t, &Fmt::new(cfg.font_size), 0, Align::Right, false);
            continue;
        }
        // 带编号的小标题同样要缩进: 嵌套大纲里"1./2./3."常常是第三、四级,
        // 顶格写就跟最外层的"1./2./3."分不出谁管谁了
        if let Some(lv) = heading_level(&t) {
            doc.para(
                &t,
                &Fmt::new(12.0 - lv as f32).bold(true),
                ind,
                Align::Left,
                false,
            );
            continue;
        }
        // 短行 + 无收尾标点 + 下一行是长正文 => 无编号小标题
        let nxt_long = paras
            .get(i + 1)
            .map(|n| n.rx1 > cfg.full_line)
            .unwrap_or(false);
        if p.rx1 < 0.58
            && !end_punct().is_match(&t)
            && t.chars().count() < 60
            && !num_start().is_match(&t)
            && nxt_long
        {
            doc.para(
                &t,
                &Fmt::new(cfg.font_size).bold(true),
                0,
                Align::Left,
                false,
            );
            continue;
        }
        if p.center {
            doc.para(&t, &Fmt::new(13.0).bold(true), 0, Align::Center, false);
            continue;
        }
        doc.para(&t, &Fmt::new(cfg.font_size), ind, Align::Left, false);
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
        &crate::tr!(crate::i18n::K::MarkPage, page_no),
        &Fmt::new(8.0).color("999999"),
        0,
        Align::Center,
        false,
    );
}

/// 单页失败: 留一条醒目占位, 其余页照常输出
pub fn page_failed(doc: &mut Docx, page_no: usize, err: &str, st: &mut State) {
    doc.para(
        &crate::tr!(crate::i18n::K::MarkFailed, page_no, err),
        &Fmt::new(10.5).bold(true).color("C03030"),
        0,
        Align::Left,
        false,
    );
    // 断了就别再往上一页的表里接。但缩进档位表得留着 —— 它攒的是全文的
    // 左边界, 跟哪一页坏了没关系。整个 State 一起清掉的话, 零点会从空表重算,
    // 失败点后面每一段的缩进都跟前面对不上。手机上尤其容易踩: 扫二十页糊掉
    // 一页是常事。
    *st = State {
        ind: std::mem::take(&mut st.ind),
        ..State::default()
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Box2;

    /// 一行只关心它从哪开始, 别的字段给个能过的值就行
    fn line(rx0: f32, ry0: f32) -> Line {
        let (rx1, ry1) = (rx0 + 0.3, ry0 + 0.012);
        let b = Box2 {
            t: "x".into(),
            x0: rx0,
            y0: ry0,
            x1: rx1,
            y1: ry1,
            s: 1.0,
            rx0,
            rx1,
            ry0,
            ry1,
        };
        Line {
            y0: ry0,
            y1: ry1,
            items: vec![b],
            rx0,
            rx1,
            ry0,
            h: 0.012,
            cx0: rx0,
        }
    }

    fn page(xs: &[f32]) -> Page {
        let lines = xs
            .iter()
            .enumerate()
            .map(|(i, &x)| line(x, 0.1 + i as f32 * 0.02))
            .collect();
        Page {
            blocks: vec![Block::Text(lines)],
            header: vec![],
            footer: vec![],
            w: 1810.0,
            h: 2558.0,
        }
    }

    /// 四级大纲一级一格 —— 缩进取自 Conclusion-for-QA 实测
    #[test]
    fn nested_outline_gets_one_level_per_step() {
        let cfg = Config::default();
        let mut ind = Vec::new();
        merge_indents(
            &mut ind,
            &page(&[0.046, 0.046, 0.046, 0.079, 0.107, 0.142]),
            &cfg,
        );
        let base = indent_base(&ind);
        assert_eq!(indent_of(0.046, base, &cfg), 0);
        assert_eq!(indent_of(0.079, base, &cfg), 1);
        assert_eq!(indent_of(0.107, base, &cfg), 2);
        assert_eq!(indent_of(0.142, base, &cfg), 3);
    }

    /// 某一页整页都是二级往下时, 零点不能跟着往右挪
    ///
    /// Conclusion-for-QA 第 2 页就是这样(唯一一条一级的落在页脚被丢了)。
    /// 按页各算各的, 同一级的条目第 1 页缩一格、第 2 页顶格, 翻页就错位。
    #[test]
    fn page_without_the_outermost_level_keeps_the_zero_point() {
        let cfg = Config::default();
        let mut ind = Vec::new();
        merge_indents(&mut ind, &page(&[0.046, 0.046, 0.046, 0.079, 0.107]), &cfg);
        merge_indents(&mut ind, &page(&[0.079, 0.079, 0.079, 0.107, 0.142]), &cfg);
        let base = indent_base(&ind);
        assert_eq!(indent_of(0.079, base, &cfg), 1, "第 2 页的二级还得是二级");
        assert_eq!(indent_of(0.142, base, &cfg), 3);
    }

    /// 零点会随着往后翻页左移 —— 所以扫描和排版必须分两趟
    ///
    /// 最外层第 2 页才出现: 只扫完第 1 页时零点是 0.079, 全扫完才是 0.046。
    /// 边扫边排的话, 第 1 页那几行会按 0.079 算, 整页凭空少缩一格
    #[test]
    fn the_zero_point_is_not_final_until_every_page_is_scanned() {
        let cfg = Config::default();
        let mut ind = Vec::new();
        merge_indents(&mut ind, &page(&[0.079, 0.079, 0.079, 0.107, 0.142]), &cfg);
        assert_eq!(indent_of(0.079, indent_base(&ind), &cfg), 0);
        merge_indents(&mut ind, &page(&[0.046, 0.046, 0.046, 0.079, 0.107]), &cfg);
        assert_eq!(
            indent_of(0.079, indent_base(&ind), &cfg),
            1,
            "扫完第 2 页, 同一个横坐标该缩一格了"
        );
    }

    /// 偶发的歪行不能把整篇的零点拽走
    ///
    /// 96 页里只要有一行识别框往左歪了一次, 取最小值就会让后面每一段凭空多缩几级
    #[test]
    fn a_one_off_stray_line_does_not_move_the_zero_point() {
        let cfg = Config::default();
        let mut ind = Vec::new();
        merge_indents(
            &mut ind,
            &page(&[0.010, 0.120, 0.120, 0.120, 0.155, 0.155, 0.155]),
            &cfg,
        );
        let base = indent_base(&ind);
        assert_eq!(indent_of(0.120, base, &cfg), 0, "正文还是顶格");
        assert_eq!(indent_of(0.155, base, &cfg), 1);
    }

    /// 抖动几千分之一还算同一档
    #[test]
    fn jitter_within_a_step_stays_one_level() {
        let cfg = Config::default();
        let mut ind = Vec::new();
        merge_indents(&mut ind, &page(&[0.120, 0.123, 0.118, 0.152, 0.155]), &cfg);
        assert_eq!(ind.len(), 2, "该聚成两档");
        assert_eq!(ind[0].1, 3);
    }

    /// 缩到最深也不能把正文挤没 —— ind_max 封顶
    #[test]
    fn far_right_text_is_capped() {
        let cfg = Config::default();
        assert_eq!(indent_of(0.90, 0.12, &cfg), cfg.ind_max);
    }

    /// 右栏正文按量到的位置缩, 不再跟左栏挤在一起
    ///
    /// 3#线 双语对照页的右栏起点在页宽 0.53, 零点 0.121。原先 ind_max=5 把
    /// 0.175 以外的一律拍平, 96 页里 269 段全落在同一级上
    #[test]
    fn the_right_hand_column_keeps_its_own_indent() {
        let cfg = Config::default();
        assert_eq!(indent_of(0.533, 0.121, &cfg), 12);
        assert!(
            !is_right_aligned(0.533, 0.743, 0.121, &cfg),
            "右栏正文不是靠右摆的"
        );
    }

    /// 缩到顶还放不下的短行是靠右摆的 —— 右上角页眉的页码就是这样
    #[test]
    fn a_short_line_past_the_last_level_is_right_aligned() {
        let cfg = Config::default();
        // 3#线 右上角: "附件1供货范围页码 5 /7"
        assert!(is_right_aligned(0.876, 0.929, 0.121, &cfg));
        // "Page 2 /31"
        assert!(is_right_aligned(0.835, 0.923, 0.121, &cfg));
    }

    /// 长段落缩到那么右是右栏正文被并串了, 不能当靠右摆的处理
    #[test]
    fn a_long_line_out_there_is_not_right_aligned() {
        let cfg = Config::default();
        assert!(!is_right_aligned(0.531, 0.926, 0.121, &cfg));
    }
}
