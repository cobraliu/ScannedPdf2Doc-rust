//! .xlsx 输出 —— 与 docx 并列的另一条路径, 只导表格
//!
//! 续表不另起一张、也不插页码行 —— 一张表在 Excel 里得是连续矩形, 中间夹一行
//! 标记, 筛选和透视就废了。页码改写在表头上方那行标记里, 变成「原第 53–56 页」。
//! 因为要"回头改", 标记行的内容先记在 `marks` 里, 收尾时才真正写进工作表。

use anyhow::Result;
use regex::Regex;
use rust_xlsxwriter::{Color, Format, FormatAlign, Workbook};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use crate::layout::grid::Grid;
use crate::layout::line::FRAC_SEP;
use crate::layout::para::{build_rows, is_header_row};
use crate::layout::{disp_w, Block, Page};

/// 把分式占位摊平成一行文本: Excel 单元格里没有 OMML 这回事
pub fn flat_text(t: &str) -> String {
    let parts: Vec<&str> = t.split(FRAC_SEP).collect();
    if parts.len() == 1 {
        return t.to_string();
    }
    static SIMPLE: OnceLock<Regex> = OnceLock::new();
    let simple = SIMPLE.get_or_init(|| Regex::new(r"^[\w.]+$").unwrap());
    let wrap = |s: &str| {
        if simple.is_match(s) {
            s.to_string()
        } else {
            format!("({s})")
        }
    };
    let mut out = String::new();
    let mut i = 0;
    while i < parts.len() {
        out.push_str(parts[i]);
        if i + 2 < parts.len() {
            out.push_str(&format!("{}/{}", wrap(parts[i + 1]), wrap(parts[i + 2])));
        }
        i += 3;
    }
    out
}

/// 能当数字用的存成数字, 其余存文本; 返回 (值, 数字格式, 文本)
///
/// 只认 10 位以内、无前导零的整数/小数。合同编号、型号、长串数字一旦被 Excel
/// 当成数字, 会掉前导零或变成科学计数法, 宁可留成文本。小数位数按原件给一个
/// 数字格式, 免得 2060000.00 显示成 2060000。
fn cell_val(t: &str) -> (Option<f64>, Option<String>, String) {
    static NUM: OnceLock<Regex> = OnceLock::new();
    static LEAD0: OnceLock<Regex> = OnceLock::new();
    let num = NUM.get_or_init(|| Regex::new(r"^-?\d{1,10}(\.\d+)?$").unwrap());
    let lead0 = LEAD0.get_or_init(|| Regex::new(r"^-?0\d").unwrap());
    let t = flat_text(t).trim().to_string();
    if num.is_match(&t) && !lead0.is_match(&t) {
        if let Ok(v) = t.parse::<f64>() {
            let fmt = t
                .split_once('.')
                .map(|(_, frac)| format!("0.{}", "0".repeat(frac.len())));
            return (Some(v), fmt, t);
        }
    }
    (None, None, t)
}

fn mark(a: usize, b: usize) -> String {
    if a == b {
        crate::tr!(crate::i18n::K::MarkPage, a)
    } else {
        crate::tr!(crate::i18n::K::MarkPages, a, b)
    }
}

/// 一个待写的单元格
struct Put {
    row: u32,
    col: u16,
    val: Option<f64>,
    text: String,
    fmt: Format,
    /// Some((末行, 末列)) 表示这一格要合并
    merge: Option<(u32, u16)>,
}

/// 跨页续表的记忆
struct St {
    row: u32,
    n: usize,
    maxc: usize,
    /// 列 -> 该列最长内容的显示宽度
    w: HashMap<usize, usize>,
    /// 当前标记在 marks 里的下标 + 它覆盖的起始页
    mark: Option<usize>,
    p0: usize,
    tbl: Option<(Vec<f32>, usize, bool)>, // (列起点, 页号, 有表头)
    grid: Option<(Vec<f32>, usize)>,      // (列边界相对坐标, 页号)
}

pub struct Book {
    title: String,
    puts: Vec<Put>,
    /// (行号, 合并列数, 文字) —— 页码范围会被后来的续表改写
    marks: Vec<(u32, usize, String)>,
    st: St,
}

impl Book {
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_string(),
            puts: Vec::new(),
            marks: Vec::new(),
            st: St {
                row: 2, // 第 1 行是标题, 第 2 行空着
                n: 0,
                maxc: 1,
                w: HashMap::new(),
                mark: None,
                p0: 0,
                tbl: None,
                grid: None,
            },
        }
    }

    /// 已导出几张表
    pub fn tables(&self) -> usize {
        self.st.n
    }

    pub fn add_page(&mut self, page: &Page, page_no: usize) {
        let blocks = &page.blocks;
        for (bi, blk) in blocks.iter().enumerate() {
            match blk {
                Block::Grid(g) => {
                    let cont = bi == 0 && self.grid_continues(g, page_no);
                    self.sheet_grid(g, page_no, cont);
                }
                Block::Table(lines, starts) => {
                    let rows = build_rows(lines, starts);
                    // 只有一行且首列为空 = 上页续行漂过来的孤立文字, 不是真表格
                    if rows.is_empty() || (rows.len() == 1 && rows[0][0].is_empty()) {
                        continue;
                    }
                    let cont = bi == 0 && self.tbl_continues(starts, page_no);
                    self.sheet_table(&rows, starts, page_no, cont);
                }
                Block::Text(_) => {}
            }
        }
        // 这里不清 st.grid/st.tbl: 续表判据只看"上一页 + 列位对得上", 表格
        // 后面跟着正文照样能接。清掉就把跨页的长表切成好几张。
    }

    fn grid_continues(&self, g: &Grid, page_no: usize) -> bool {
        self.st
            .grid
            .as_ref()
            .map(|(rxs, p)| {
                *p + 1 == page_no
                    && rxs.len() == g.rxs.len()
                    && rxs
                        .iter()
                        .zip(&g.rxs)
                        .map(|(a, b)| (a - b).abs())
                        .fold(0.0f32, f32::max)
                        < 0.02
            })
            .unwrap_or(false)
    }

    fn tbl_continues(&self, starts: &[f32], page_no: usize) -> bool {
        self.st
            .tbl
            .as_ref()
            .map(|(s, p, _)| {
                *p + 1 == page_no
                    && s.len() == starts.len()
                    && s.iter()
                        .zip(starts)
                        .map(|(a, b)| (a - b).abs())
                        .fold(0.0f32, f32::max)
                        < 0.05
            })
            .unwrap_or(false)
    }

    /// 新表: 表与表之间空一行, 上面压一条页码标记
    ///
    /// 标记行跨整张表的宽度合并居中: 它是表外的一条分隔带, 不是数据, 合并了
    /// 才不会看着像"第一列有个奇怪的值"。
    fn begin_mark(&mut self, page_no: usize, ncols: usize) {
        if self.st.row > 2 {
            self.st.row += 1;
        }
        self.marks
            .push((self.st.row, ncols, mark(page_no, page_no)));
        self.st.mark = Some(self.marks.len() - 1);
        self.st.maxc = self.st.maxc.max(ncols);
        self.st.row += 1;
        self.st.p0 = page_no;
        self.st.n += 1;
    }

    /// 续表: 只改标记的页码范围
    fn extend_mark(&mut self, page_no: usize) {
        if let Some(i) = self.st.mark {
            self.marks[i].2 = mark(self.st.p0, page_no);
        }
    }

    fn bump_w(&mut self, col: usize, t: &str) {
        let e = self.st.w.entry(col).or_insert(0);
        *e = (*e).max(disp_w(t));
    }

    /// 框线表格写进工作表: 原件合并的格子在这里也合并
    ///
    /// 只合并"有字"的块 —— 空白区照原样合并只会留下一大片死区, 反正没边框,
    /// 看不出区别。
    fn sheet_grid(&mut self, g: &Grid, page_no: usize, cont: bool) {
        if cont {
            self.extend_mark(page_no);
        } else {
            self.begin_mark(page_no, g.ncols());
        }
        let head = !cont && is_header_row(&g.rows());
        let r0 = self.st.row;
        let cells = g.cells.clone();
        for cell in &cells {
            if cell.text.is_empty() {
                continue;
            }
            let (val, nfmt, text) = cell_val(&cell.text);
            let merged = cell.h > 1 || cell.w > 1;
            let mut f = Format::new().set_text_wrap();
            if merged {
                // 合并块居中: 跨行跨列的多半是标题/分类名; 长句子仍靠左
                f = f.set_align(FormatAlign::VerticalCenter);
                f = f.set_align(if cell.text.chars().count() <= 12 {
                    FormatAlign::Center
                } else {
                    FormatAlign::Left
                });
            } else {
                f = f.set_align(FormatAlign::Top);
            }
            if let Some(n) = &nfmt {
                f = f.set_num_format(n.clone());
            }
            if head && cell.r == 0 {
                f = f.set_bold().set_background_color(Color::RGB(0xEF_EF_EF));
            }
            self.puts.push(Put {
                row: r0 + cell.r as u32,
                col: cell.c as u16,
                val,
                text: text.clone(),
                fmt: f,
                merge: merged.then(|| {
                    (
                        (r0 + (cell.r + cell.h) as u32) - 1,
                        (cell.c + cell.w) as u16 - 1,
                    )
                }),
            });
            if !merged {
                self.bump_w(cell.c, &text);
            }
        }
        self.st.row = r0 + g.nrows() as u32;
        self.st.grid = Some((g.rxs.clone(), page_no));
        self.st.tbl = None; // 框线表不跟无框线表互相续接
    }

    fn sheet_table(&mut self, rows: &[Vec<String>], starts: &[f32], page_no: usize, cont: bool) {
        let head = if cont {
            self.extend_mark(page_no);
            self.st.tbl.as_ref().map(|(_, _, h)| *h).unwrap_or(false)
        } else {
            self.begin_mark(page_no, starts.len());
            is_header_row(rows)
        };
        let bold_first = !head && starts.len() == 2; // 标签—值: 左列即标签
        for (n, txt) in rows.iter().enumerate() {
            for (c, t) in txt.iter().enumerate() {
                if t.is_empty() {
                    continue;
                }
                let (val, nfmt, text) = cell_val(t);
                let mut f = Format::new().set_text_wrap().set_align(FormatAlign::Top);
                if let Some(nf) = &nfmt {
                    f = f.set_num_format(nf.clone());
                }
                if head && n == 0 && !cont {
                    f = f.set_bold().set_background_color(Color::RGB(0xEF_EF_EF));
                } else if bold_first && c == 0 {
                    f = f.set_bold();
                }
                self.puts.push(Put {
                    row: self.st.row,
                    col: c as u16,
                    val,
                    text: text.clone(),
                    fmt: f,
                    merge: None,
                });
                self.bump_w(c, &text);
            }
            self.st.row += 1;
        }
        self.st.tbl = Some((starts.to_vec(), page_no, head));
    }

    /// 单页失败不能丢掉整篇: 留一条醒目占位, 其余页照常导出
    pub fn page_failed(&mut self, page_no: usize, err: &str) {
        self.puts.push(Put {
            row: self.st.row,
            col: 0,
            val: None,
            text: crate::tr!(crate::i18n::K::MarkFailed, page_no, err),
            fmt: Format::new()
                .set_bold()
                .set_font_color(Color::RGB(0xC0_30_30)),
            merge: None,
        });
        self.st.row += 2;
        self.st.tbl = None; // 断了就别再往上一页的表里接
        self.st.grid = None;
    }

    pub fn save(self, path: &Path) -> Result<usize> {
        let mut wb = Workbook::new();
        let ws = wb.add_worksheet();
        ws.set_name(crate::i18n::t(crate::i18n::K::SheetName))?;

        let mut tfmt = Format::new().set_bold().set_font_size(14);
        if self.st.maxc > 1 {
            // 标题跟着最宽的那张表一起居中
            tfmt = tfmt.set_align(FormatAlign::Center);
            ws.merge_range(0, 0, 0, self.st.maxc as u16 - 1, &self.title, &tfmt)?;
        } else {
            ws.write_string_with_format(0, 0, &self.title, &tfmt)?;
        }

        let mfmt = Format::new()
            .set_font_size(9)
            .set_font_color(Color::RGB(0x99_99_99))
            .set_align(FormatAlign::Center)
            .set_align(FormatAlign::VerticalCenter);
        for (row, ncols, text) in &self.marks {
            if *ncols > 1 {
                ws.merge_range(*row, 0, *row, *ncols as u16 - 1, text, &mfmt)?;
            } else {
                ws.write_string_with_format(*row, 0, text, &mfmt)?;
            }
        }

        for p in &self.puts {
            if let Some((r2, c2)) = p.merge {
                // 合并区先铺一遍格式, 再把真正的值写进左上角 —— merge_range
                // 只收字符串, 数字得单独补一次
                ws.merge_range(p.row, p.col, r2, c2, "", &p.fmt)?;
            }
            match p.val {
                Some(v) => ws.write_number_with_format(p.row, p.col, v, &p.fmt)?,
                None => ws.write_string_with_format(p.row, p.col, &p.text, &p.fmt)?,
            };
        }

        // 列宽按该列最长的一格给, 但封顶 42 —— 整格都开了自动换行, 再宽只会让
        // 一列独占屏幕, 反而看不见右边的列
        // 排序只为让写出来的 xml 每次都一样 —— 设宽本身跟顺序无关, 但
        // HashMap 的遍历顺序每个进程都不同, 不排的话同一份输入产出的文件
        // 字节不一致, 想做回归对比就没法比
        let mut ws_widths: Vec<_> = self.st.w.iter().map(|(&c, &w)| (c, w)).collect();
        ws_widths.sort_unstable();
        for (c, w) in ws_widths {
            ws.set_column_width(c as u16, (w + 2).clamp(8, 42) as f64)?;
        }
        if self.st.n == 0 {
            ws.write_string(2, 0, crate::i18n::t(crate::i18n::K::NoTableFound))?;
        }
        // 走 save_to_writer 只为拿 create_new: Workbook::save 内部是 File::create,
        // 会闷声覆盖。调用方已经挑过空名字, 这里是第二道闸
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        wb.save_to_writer(file)?;
        Ok(self.st.n)
    }
}
