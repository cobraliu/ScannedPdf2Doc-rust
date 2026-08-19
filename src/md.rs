//! 块序列 -> Markdown
//!
//! 跟 docx 那条路走的是同一批块, 但取舍不一样: Markdown 表达不了缩进档位、
//! 字号、颜色, 也没有合并单元格。能留的是**结构** —— 标题分级、清单层级、
//! 表格的行列、图片的位置。留不住的那些, 与其用一堆 HTML 标签硬凑, 不如
//! 让它保持是一份能直接读、能进笔记本、能喂给别的工具的纯文本。
//!
//! 图片单独落在 `<同名>.assets/` 里, 用相对路径引 —— Typora、Obsidian、
//! GitHub 都认这个摆法。

use anyhow::Result;
use std::path::Path;

use crate::config::Config;
use crate::layout::para::{
    build_rows, bullet, heading_level, is_header_row, mark_bullets, merge_paras,
};
use crate::layout::{Block, Line, Page};
use crate::render::{indent_base, indent_of, State};

/// 图片目录名要等 `save` 拿到最终文件名才定得下来(重名时会改名), 正文里
/// 先埋这个记号, 落盘时一次换掉
const ASSETS: &str = "%ASSETS%";

pub struct Book {
    md: String,
    /// 按出现顺序编号的图, 文件名就是 fig-1.png、fig-2.png…
    figs: Vec<Vec<u8>>,
    /// 上一页结尾那张表: 列数 + 页号。对得上就接着往下写行, 不另起一张
    tbl: Option<(usize, usize)>,
}

impl Book {
    pub fn new(title: &str) -> Self {
        Self {
            md: format!("# {}\n\n", esc(title)),
            figs: Vec::new(),
            tbl: None,
        }
    }

    /// 「原第 N 页」在 Markdown 里写成注释: 渲染出来看不见, 但文件里查得到,
    /// 对回原件时用得上
    pub fn page_marker(&mut self, page: &Page, page_no: usize) {
        if self.continues(&page.blocks, page_no) {
            return;
        }
        self.md.push_str(&format!(
            "<!-- {} -->\n\n",
            crate::tr!(crate::i18n::K::MarkPage, page_no)
        ));
    }

    pub fn page_failed(&mut self, page_no: usize, err: &str) {
        self.md.push_str(&format!(
            "> **{}**\n\n",
            esc(&crate::tr!(crate::i18n::K::MarkFailed, page_no, err))
        ));
        self.tbl = None;
    }

    pub fn add_page(&mut self, page: &Page, page_no: usize, cfg: &Config, st: &State) {
        let base = indent_base(st.indents());
        for (bi, blk) in page.blocks.iter().enumerate() {
            match blk {
                Block::Text(lines) => {
                    self.text(lines, cfg, base);
                    self.tbl = None;
                }
                Block::Figure(f) => {
                    self.figs.push(f.png.clone());
                    self.md
                        .push_str(&format!("![]({ASSETS}/fig-{}.png)\n\n", self.figs.len()));
                    self.tbl = None;
                }
                Block::Table(lines, starts) => {
                    let rows = build_rows(lines, starts);
                    // 只有一行且首列为空 = 上页续行漂过来的孤立文字, 不是真表格
                    if rows.is_empty() || (rows.len() == 1 && rows[0][0].is_empty()) {
                        continue;
                    }
                    let cont = bi == 0 && self.same_table(starts.len(), page_no);
                    self.table(&rows, is_header_row(&rows), cont);
                    self.tbl = Some((starts.len(), page_no));
                }
                Block::Grid(g) => {
                    let rows = g.rows();
                    if rows.is_empty() {
                        continue;
                    }
                    let cont = bi == 0 && self.same_table(g.ncols(), page_no);
                    self.table(&rows, is_header_row(&rows), cont);
                    self.tbl = Some((g.ncols(), page_no));
                }
            }
        }
    }

    fn continues(&self, blocks: &[Block], page_no: usize) -> bool {
        match blocks.first() {
            Some(Block::Table(_, starts)) => self.same_table(starts.len(), page_no),
            Some(Block::Grid(g)) => self.same_table(g.ncols(), page_no),
            _ => false,
        }
    }

    /// 续表判据比 docx 那边松: Markdown 的表没有列宽, 列数对得上就接得住
    fn same_table(&self, ncols: usize, page_no: usize) -> bool {
        self.tbl == Some((ncols, page_no - 1))
    }

    fn text(&mut self, lines: &[Line], cfg: &Config, base: f32) {
        for p in mark_bullets(merge_paras(lines, cfg), cfg) {
            let t = p.text.trim().to_string();
            if t.is_empty() {
                continue;
            }
            let ind = indent_of(p.cx0, base, cfg) as usize;
            if let Some(m) = bullet().find(&t) {
                let pad = "  ".repeat(ind);
                self.md
                    .push_str(&format!("{pad}- {}\n", esc(t[m.end()..].trim())));
                continue;
            }
            if p.bullet {
                self.md
                    .push_str(&format!("{}- {}\n", "  ".repeat(ind), esc(&t)));
                continue;
            }
            // 一级留给文档标题, 所以从 ## 起
            if let Some(lv) = heading_level(&t) {
                self.md.push_str(&format!(
                    "\n{} {}\n\n",
                    "#".repeat(lv as usize + 1),
                    esc(&t)
                ));
                continue;
            }
            if p.center {
                self.md.push_str(&format!("\n## {}\n\n", esc(&t)));
                continue;
            }
            self.md.push_str(&format!("{}\n\n", esc(&t)));
        }
        if !self.md.ends_with("\n\n") {
            self.md.push('\n');
        }
    }

    /// `cont` 为真时不写表头分隔线, 直接续上一页那张表的行
    fn table(&mut self, rows: &[Vec<String>], header: bool, cont: bool) {
        let nc = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if nc == 0 {
            return;
        }
        if cont {
            // 上一张表写完跟了个空行, 抹掉才接得上
            self.md.truncate(self.md.trim_end().len());
            self.md.push('\n');
            for r in rows {
                self.md.push_str(&row(r, nc));
            }
        } else {
            // Markdown 的表必须有分隔线, 原件没表头就拿一行空的顶上去
            let (head, body) = if header && !rows.is_empty() {
                (rows[0].clone(), &rows[1..])
            } else {
                (vec![String::new(); nc], rows)
            };
            self.md.push_str(&row(&head, nc));
            self.md.push_str(&format!("|{}\n", " --- |".repeat(nc)));
            for r in body {
                self.md.push_str(&row(r, nc));
            }
        }
        self.md.push('\n');
    }

    /// 落盘: `.md` 一份, 图片进同名的 `.assets/`; 返回写了几张图
    pub fn save(&self, path: &Path) -> Result<usize> {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "DOC".into());
        let dir = format!("{stem}.assets");
        if !self.figs.is_empty() {
            let d = path.with_file_name(&dir);
            std::fs::create_dir_all(&d)?;
            for (i, png) in self.figs.iter().enumerate() {
                std::fs::write(d.join(format!("fig-{}.png", i + 1)), png)?;
            }
        }
        // 链接里的空格得转义, 否则文件名带空格的时候引不到
        let md = squeeze(&self.md.replace(ASSETS, &dir.replace(' ', "%20")));
        std::fs::write(path, md)?;
        Ok(self.figs.len())
    }
}

/// 连着三个以上换行压成两个 —— 段落之间空一行就够, 空三四行是拼接的痕迹
fn squeeze(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut nl = 0;
    for c in t.chars() {
        if c == '\n' {
            nl += 1;
            if nl > 2 {
                continue;
            }
        } else {
            nl = 0;
        }
        out.push(c);
    }
    out
}

fn row(cells: &[String], nc: usize) -> String {
    let mut s = String::from("|");
    for i in 0..nc {
        s.push(' ');
        s.push_str(&cell(cells.get(i).map(String::as_str).unwrap_or("")));
        s.push_str(" |");
    }
    s.push('\n');
    s
}

/// 表格里的一格: 竖线会把格子切开, 换行会把表截断
fn cell(t: &str) -> String {
    esc(t).replace('|', "\\|").replace('\n', "<br>")
}

/// 只挡会把整段读歪的那几个行首记号
///
/// 不做全量转义: OCR 出来的中文里 `*`、`_`、`#` 出现在句中不成对, 挨个加反斜杠
/// 只会让正文长出一堆 `\`, 读起来比偶尔错一次斜体还糟。真正会出事的是行首 ——
/// 行首的 `#` 变标题、`>` 变引用、`-` 变清单, 那是整段变形。
fn esc(t: &str) -> String {
    let t = t.trim_end();
    match t.chars().next() {
        Some('#') | Some('>') | Some('|') => format!("\\{t}"),
        _ => t.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::grid::{Cell, Grid};
    use crate::layout::Box2;

    fn line(t: &str, rx0: f32, ry0: f32) -> Line {
        let (rx1, ry1) = (rx0 + 0.4, ry0 + 0.012);
        let b = Box2 {
            t: t.into(),
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

    fn text_page(rows: &[(&str, f32)]) -> Page {
        let lines = rows
            .iter()
            .enumerate()
            .map(|(i, (t, x))| line(t, *x, 0.1 + i as f32 * 0.05))
            .collect();
        Page {
            blocks: vec![Block::Text(lines)],
            header: vec![],
            footer: vec![],
            w: 1800.0,
            h: 2500.0,
        }
    }

    fn grid(rows: &[&[&str]]) -> Grid {
        let nc = rows[0].len();
        let cells = rows
            .iter()
            .enumerate()
            .flat_map(|(r, row)| {
                row.iter().enumerate().map(move |(c, t)| Cell {
                    r,
                    c,
                    h: 1,
                    w: 1,
                    text: (*t).into(),
                    edges: [true; 4],
                })
            })
            .collect();
        Grid {
            xs: (0..=nc).map(|i| i as f32 * 100.0).collect(),
            ys: (0..=rows.len()).map(|i| i as f32 * 40.0).collect(),
            cells,
            merged: 0,
            x0: 0.0,
            y0: 0.0,
            x1: nc as f32 * 100.0,
            y1: rows.len() as f32 * 40.0,
            rxs: (0..=nc).map(|i| i as f32 / nc as f32).collect(),
        }
    }

    fn grid_page(g: Grid) -> Page {
        Page {
            blocks: vec![Block::Grid(g)],
            header: vec![],
            footer: vec![],
            w: 1800.0,
            h: 2500.0,
        }
    }

    fn render(pages: Vec<Page>) -> String {
        let cfg = Config::default();
        let mut st = State::default();
        for p in &pages {
            crate::render::scan_indents(&mut st, p, &cfg);
        }
        let mut b = Book::new("合同");
        for (i, p) in pages.iter().enumerate() {
            b.add_page(p, i + 1, &cfg, &st);
        }
        b.md.clone()
    }

    #[test]
    fn the_title_becomes_the_only_first_level_heading() {
        let md = render(vec![text_page(&[("一、总则", 0.1)])]);
        assert!(md.starts_with("# 合同\n"), "{md}");
        // 正文里的编号标题从二级起, 不跟文档标题抢。「一、」在标题阶梯上
        // 是第二档, 所以是三级
        assert!(md.contains("\n### 一、总则\n"), "{md}");
        assert_eq!(md.matches("# 合同").count(), 1, "{md}");
    }

    #[test]
    fn a_grid_turns_into_a_pipe_table() {
        let md = render(vec![grid_page(grid(&[&["名称", "数量"], &["电缆", "20"]]))]);
        assert!(md.contains("| 名称 | 数量 |\n"), "{md}");
        assert!(md.contains("| --- | --- |\n"), "{md}");
        assert!(md.contains("| 电缆 | 20 |\n"), "{md}");
    }

    /// 跨页的长表在 Markdown 里也得是一张, 不然 96 页的合同出一百来张两行表
    #[test]
    fn a_table_running_across_pages_stays_one_table() {
        let md = render(vec![
            grid_page(grid(&[&["名称", "数量"], &["电缆", "20"]])),
            grid_page(grid(&[&["桥架", "35"]])),
        ]);
        assert_eq!(
            md.matches("| --- | --- |").count(),
            1,
            "只该有一条分隔线\n{md}"
        );
        assert!(md.contains("| 桥架 | 35 |"), "{md}");
    }

    /// 中间隔了正文就不是同一张表了
    #[test]
    fn text_in_between_ends_the_table() {
        let md = render(vec![
            grid_page(grid(&[&["名称", "数量"], &["电缆", "20"]])),
            text_page(&[("以下为补充条款。", 0.1)]),
            grid_page(grid(&[&["桥架", "35"]])),
        ]);
        assert_eq!(md.matches("| --- | --- |").count(), 2, "{md}");
    }

    /// 格子里出现竖线会把这一行切成别的列数, 整张表就散了
    #[test]
    fn a_pipe_inside_a_cell_does_not_break_the_row() {
        let md = render(vec![grid_page(grid(&[&["规格", "值"], &["A|B", "3"]]))]);
        let row = md.lines().find(|l| l.contains("3")).expect("找不到那一行");
        assert_eq!(row.matches("| ").count(), 2, "列数被切多了: {row}");
        assert!(row.contains("A\\|B"), "{row}");
    }

    #[test]
    fn a_figure_is_linked_into_the_assets_folder() {
        let page = Page {
            blocks: vec![Block::Figure(crate::layout::Fig {
                png: vec![1, 2, 3],
                x0: 0.0,
                y0: 0.0,
                x1: 100.0,
                y1: 100.0,
            })],
            header: vec![],
            footer: vec![],
            w: 1800.0,
            h: 2500.0,
        };
        let cfg = Config::default();
        let st = State::default();
        let mut b = Book::new("配套");
        b.add_page(&page, 1, &cfg, &st);

        let dir = std::env::temp_dir().join(format!("pdf2doc-md-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let out = dir.join("配套.md");
        assert_eq!(b.save(&out).expect("落盘"), 1);

        let md = std::fs::read_to_string(&out).expect("读回");
        assert!(md.contains("![](配套.assets/fig-1.png)"), "{md}");
        assert_eq!(
            std::fs::read(dir.join("配套.assets/fig-1.png")).expect("图落地"),
            vec![1, 2, 3]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blank_lines_do_not_pile_up() {
        assert_eq!(squeeze("a\n\n\n\nb"), "a\n\nb");
        assert_eq!(squeeze("a\n\nb"), "a\n\nb");
    }

    /// 行首的 `#` 不挡住就把一整段变成标题
    #[test]
    fn a_hash_at_the_start_of_a_line_is_escaped() {
        assert_eq!(esc("#3 线电缆"), "\\#3 线电缆");
        // 句中的不用管: 挨个转义只会让中文正文长出一堆反斜杠
        assert_eq!(esc("回路 #3 已接"), "回路 #3 已接");
    }
}
