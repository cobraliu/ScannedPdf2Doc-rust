//! 版面重建 —— 规则引擎, 与 Python 版 convert.py 的第 1~5 节一一对应
//!
//! 这一层没有任何第三方依赖: 输入是 OCR 给的一堆带坐标的文字块, 输出是
//! "这一页由哪些段落、哪些表格构成"。换语言不影响判据, 阈值全部照搬。

pub mod block;
pub mod grid;
pub mod line;
pub mod noise;
pub mod para;

use crate::config::Config;
use crate::imgutil::Gray;
use crate::ocr::Item;

/// 带相对坐标的文字块 —— 相对坐标是判据的通用语言, 换页宽也不用改阈值
#[derive(Debug, Clone)]
pub struct Box2 {
    pub t: String,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub s: f32,
    pub rx0: f32,
    pub rx1: f32,
    pub ry0: f32,
    pub ry1: f32,
}

impl Box2 {
    pub fn from_item(it: &Item, w: f32, h: f32, text: String) -> Self {
        Self {
            t: text,
            x0: it.x0,
            y0: it.y0,
            x1: it.x1,
            y1: it.y1,
            s: it.s,
            rx0: it.x0 / w,
            rx1: it.x1 / w,
            ry0: it.y0 / h,
            ry1: it.y1 / h,
        }
    }
}

/// 一个视觉行
#[derive(Debug, Clone)]
pub struct Line {
    pub y0: f32,
    pub y1: f32,
    pub items: Vec<Box2>,
    pub rx0: f32,
    pub rx1: f32,
    pub ry0: f32,
    pub h: f32,
    /// 内容起点: 首 item 若是窄编号, 基准取下一个 item
    pub cx0: f32,
}

impl Line {
    pub fn refresh(&mut self) {
        self.items.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap());
        self.rx0 = self.items[0].rx0;
        self.rx1 = self.items.iter().map(|i| i.rx1).fold(f32::MIN, f32::max);
        self.ry0 = self.items[0].ry0;
        self.h = self.y1 - self.y0;
        let first = &self.items[0];
        self.cx0 = if self.items.len() > 1 && (first.rx1 - first.rx0) < 0.08 {
            self.items[1].rx0
        } else {
            first.rx0
        };
    }
}

/// 一页切出来的块
#[derive(Debug, Clone)]
pub enum Block {
    /// 正文段落区
    Text(Vec<Line>),
    /// 印章 / 签名 / 插图: 原样裁下来的一块图
    Figure(Fig),
    /// 无框线表格: 行 + 列起点
    Table(Vec<Line>, Vec<f32>),
    /// 有框线表格
    Grid(grid::Grid),
}

/// 一处照原样保留的图形, 坐标是页图像素
#[derive(Debug, Clone)]
pub struct Fig {
    pub png: Vec<u8>,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// 一页的分析结果
pub struct Page {
    pub blocks: Vec<Block>,
    pub header: Vec<Box2>,
    pub footer: Vec<Box2>,
    pub w: f32,
    pub h: f32,
}

/// 一页: OCR 结果 + 页图 -> 块序列
pub fn analyze(items: &[Item], img: &Gray, cfg: &Config) -> Page {
    let (w, h) = (img.w as f32, img.h as f32);
    let (body, header, footer) = noise::drop_noise(items, w, h, cfg);

    let grids = if cfg.grid_tables {
        grid::find_grids(img, &body)
    } else {
        Vec::new()
    };

    let rest: Vec<Box2> = if grids.is_empty() {
        body.clone()
    } else {
        body.iter()
            .filter(|it| !grid::in_grid(it, &grids))
            .cloned()
            .collect()
    };
    let mut grids = grids;
    for g in grids.iter_mut() {
        let mine: Vec<Box2> = body
            .iter()
            .filter(|it| grid::in_grid(it, std::slice::from_ref(g)))
            .cloned()
            .collect();
        grid::fill_grid(g, &mine);
    }

    let lines = line::find_fracs(line::group_lines(rest, cfg), img);
    let blocks = block::weave(&lines, &grids, cfg);
    Page {
        blocks,
        header,
        footer,
        w,
        h,
    }
}

/// 这一块从哪个高度开始 —— 用来把图形按阅读顺序插进去
fn block_y0(b: &Block) -> f32 {
    match b {
        Block::Text(l) | Block::Table(l, _) => l.first().map_or(f32::MAX, |x| x.y0),
        Block::Grid(g) => g.y0,
        Block::Figure(f) => f.y0,
    }
}

impl Page {
    /// 页面上认出的框线表 —— 图形检测要拿它当排除区
    pub fn grids(&self) -> Vec<&grid::Grid> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Grid(g) => Some(g),
                _ => None,
            })
            .collect()
    }

    /// 把裁好的图形按阅读顺序插进块序列
    pub fn insert_figs(&mut self, figs: Vec<Fig>) {
        let blocks = std::mem::take(&mut self.blocks);
        self.blocks = drop_in_figs(blocks, figs);
    }
}

/// 图形按纵坐标插进块序列
///
/// 插在"第一个比它更靠下的块"前面。图形常常压在别的东西上(章盖在签名栏上),
/// 硬要不重叠是做不到的, 排进阅读顺序就够了。
fn drop_in_figs(blocks: Vec<Block>, figs: Vec<Fig>) -> Vec<Block> {
    if figs.is_empty() {
        return blocks;
    }
    let mut out: Vec<Block> = Vec::with_capacity(blocks.len() + figs.len());
    let mut it = blocks.into_iter().peekable();
    for f in figs {
        while it.peek().is_some_and(|b| block_y0(b) <= f.y0) {
            out.push(it.next().expect("peek 过了"));
        }
        out.push(Block::Figure(f));
    }
    out.extend(it);
    out
}

// ---------- 文本工具 ----------

/// 两个以上汉字就当中文
pub fn is_zh(s: &str) -> bool {
    s.chars()
        .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
        .count()
        >= 2
}

/// 全角空格换半角, 压掉连续空白
///
/// 只压**连续两个及以上**的空白(对应 Python 的 `re.sub(r'\s{2,}', ' ', t)`),
/// 单个空白原样留着 —— 关键是留住单个 `\n`: 单元格内的换行是有意义的
/// ("手动下单/自动下单"是两条并列的值), 一律压成空格就把它们粘成一行了。
pub fn clean(t: &str) -> String {
    let t = t.replace('\u{3000}', " ");
    let ch: Vec<char> = t.trim().chars().collect();
    let mut out = String::with_capacity(t.len());
    let mut i = 0;
    while i < ch.len() {
        if !ch[i].is_whitespace() {
            out.push(ch[i]);
            i += 1;
            continue;
        }
        let mut k = i;
        while k < ch.len() && ch[k].is_whitespace() {
            k += 1;
        }
        out.push(if k - i >= 2 { ' ' } else { ch[i] });
        i = k;
    }
    out
}

/// 显示宽度: 一个汉字算两列
pub fn disp_w(t: &str) -> usize {
    t.chars()
        .map(|c| if (c as u32) > 0x2e80 { 2 } else { 1 })
        .sum()
}
