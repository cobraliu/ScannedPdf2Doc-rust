//! 框线表格: 照框线还原行列, 顺带还原合并单元格
//!
//! 缺了内部框线的地方, 就是原件里的合并单元格 —— 这一条是整套判据的核心。

use super::{clean, is_zh, Box2};
use crate::config::Config;
use crate::imgutil::{connected_components, Gray};

/// 框线判深浅: 正文用 160, 框线要放宽 —— Excel 导出的浅灰边框实测才 170,
/// 而最深的底纹(蓝色表头)是 195, 阈值卡在中间
const RULE_DARK: u8 = 185;

#[derive(Debug, Clone, Copy)]
struct HSeg {
    x0: f32,
    x1: f32,
    y: f32,
}

#[derive(Debug, Clone, Copy)]
struct VSeg {
    y0: f32,
    y1: f32,
    x: f32,
}

/// 一个(可能跨行跨列的)单元格
#[derive(Debug, Clone)]
pub struct Cell {
    pub r: usize,
    pub c: usize,
    pub h: usize,
    pub w: usize,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Grid {
    pub xs: Vec<f32>,
    pub ys: Vec<f32>,
    pub cells: Vec<Cell>,
    pub merged: usize,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// 列边界的相对坐标 —— 列宽和跨页续表判定都用它
    pub rxs: Vec<f32>,
}

impl Grid {
    pub fn nrows(&self) -> usize {
        self.ys.len() - 1
    }
    pub fn ncols(&self) -> usize {
        self.xs.len() - 1
    }
    /// 文本矩阵(合并块的字写在左上格), 供表头判定用
    pub fn rows(&self) -> Vec<Vec<String>> {
        let mut m = vec![vec![String::new(); self.ncols()]; self.nrows()];
        for c in &self.cells {
            m[c.r][c.c] = c.text.clone();
        }
        m
    }
}

/// 形态学开运算取出一个方向上的框线
///
/// 厚度按"面积 ÷ 长度"算, 不能用外接框 —— 扫描件总带零点几度歪斜, 一条
/// 1500 px 的横线外接框就有七八像素高, 按外接框判会把整张表的框线全扔掉。
fn rules(bw: &crate::imgutil::Bitmap, horiz: bool, min_len: usize, max_thick: f32) -> Vec<(f32, f32, f32)> {
    let opened = bw.open_line(horiz, min_len);
    let mut out = Vec::new();
    for b in connected_components(&opened) {
        let span = if horiz { b.w } else { b.h };
        if span < min_len || b.area as f32 / span as f32 > max_thick {
            continue;
        }
        if horiz {
            out.push((b.x as f32, (b.x + b.w) as f32, b.y as f32 + b.h as f32 / 2.0));
        } else {
            out.push((b.y as f32, (b.y + b.h) as f32, b.x as f32 + b.w as f32 / 2.0));
        }
    }
    out
}

/// 只留两端都搭在横线上的竖线
///
/// 汉字的竖笔跟框线一样细一样直, 长度也能碰到阈值; 区别是笔画悬在格子中间,
/// 两头不接横线。不加这条, 一页正文能"检出"三百多条竖线。
fn anchored(vs: Vec<VSeg>, ys: &[f32], tol: f32) -> Vec<VSeg> {
    vs.into_iter()
        .filter(|v| {
            ys.iter().any(|y| (v.y0 - y).abs() <= tol) && ys.iter().any(|y| (v.y1 - y).abs() <= tol)
        })
        .collect()
}

/// 再滤一道: 压在文字框里的不是框线, 是笔画
///
/// 大号字("基本信息"这种标题)的竖笔能有半格高, 两头离上下框线都很近, 光靠
/// 锚定滤不掉。但框线画在格与格之间, 不会落进 OCR 的文字框里 —— 用这条分。
fn off_text_h(segs: Vec<HSeg>, items: &[Box2]) -> Vec<HSeg> {
    segs.into_iter()
        .filter(|s| {
            let n = (s.x1 - s.x0).max(1.0);
            let mut cover = 0.0f32;
            for it in items {
                if it.y0 - 2.0 <= s.y && s.y <= it.y1 + 2.0 {
                    cover = cover.max((s.x1.min(it.x1) - s.x0.max(it.x0)) / n);
                }
            }
            cover <= 0.8
        })
        .collect()
}

fn off_text_v(segs: Vec<VSeg>, items: &[Box2]) -> Vec<VSeg> {
    // 看"单个"文字框吃掉多少, 不是累加: 框线常贴着相邻格文字框的边走,
    // 累加会把两侧的框各算一半, 把真框线也判成笔画
    segs.into_iter()
        .filter(|s| {
            let n = (s.y1 - s.y0).max(1.0);
            let mut cover = 0.0f32;
            for it in items {
                if it.x0 - 2.0 <= s.x && s.x <= it.x1 + 2.0 {
                    cover = cover.max((s.y1.min(it.y1) - s.y0.max(it.y0)) / n);
                }
            }
            cover <= 0.8
        })
        .collect()
}

/// 一维聚类: 挨得比 tol 近的并成一档, 返回各档中心
fn cluster(mut vals: Vec<f32>, tol: f32) -> Vec<f32> {
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut groups: Vec<Vec<f32>> = Vec::new();
    for v in vals {
        match groups.last_mut() {
            Some(g) if v - *g.last().unwrap() <= tol => g.push(v),
            _ => groups.push(vec![v]),
        }
    }
    groups
        .into_iter()
        .map(|g| g.iter().sum::<f32>() / g.len() as f32)
        .collect()
}

/// v 落在 edges 划出的第几格
fn band(edges: &[f32], v: f32) -> usize {
    let mut k = 0;
    for i in 0..edges.len().saturating_sub(1) {
        if v >= edges[i] {
            k = i;
        }
    }
    k
}

/// 网格里哪些格是连在一起的: 缺了内部框线就是原件里的合并单元格
fn grid_cells(xs: &[f32], ys: &[f32], hs: &[HSeg], vs: &[VSeg], tol: f32) -> (Vec<Cell>, usize) {
    let (nr, nc) = (ys.len() - 1, xs.len() - 1);
    // (r,c) 右边有没有竖线
    let has_v = |r: usize, c: usize| -> bool {
        let (y0, y1) = (ys[r], ys[r + 1]);
        vs.iter().any(|v| {
            (v.x - xs[c + 1]).abs() <= tol && v.y1.min(y1) - v.y0.max(y0) >= 0.6 * (y1 - y0)
        })
    };
    // (r,c) 下边有没有横线
    let has_h = |r: usize, c: usize| -> bool {
        let (x0, x1) = (xs[c], xs[c + 1]);
        hs.iter().any(|h| {
            (h.y - ys[r + 1]).abs() <= tol && h.x1.min(x1) - h.x0.max(x0) >= 0.6 * (x1 - x0)
        })
    };
    let mut used = vec![vec![false; nc]; nr];
    let mut cells = Vec::new();
    let mut merged = 0usize;
    for r in 0..nr {
        for c in 0..nc {
            if used[r][c] {
                continue;
            }
            let mut w = 1;
            while c + w < nc && !used[r][c + w] && !has_v(r, c + w - 1) {
                w += 1;
            }
            let mut h = 1;
            while r + h < nr
                && !used[r + h][c]
                && (0..w).all(|k| !has_h(r + h - 1, c + k))
            {
                h += 1;
            }
            for i in r..r + h {
                for j in c..c + w {
                    used[i][j] = true;
                }
            }
            cells.push(Cell { r, c, h, w, text: String::new() });
            if h > 1 || w > 1 {
                merged += 1;
            }
        }
    }
    (cells, merged)
}

/// 页图里所有画了框线的表格; 没有框线返回空, 走原来那套几何判定
pub fn find_grids(img: &Gray, items: &[Box2]) -> Vec<Grid> {
    let (w, h) = (img.w, img.h);
    let bw = img.binarize(RULE_DARK);
    let tol = (w as f32 / 300.0).max(5.0);
    let thick = (w as f32 / 500.0).max(4.0);

    let hs: Vec<HSeg> = rules(&bw, true, (w / 40).max(40), thick)
        .into_iter()
        .map(|(x0, x1, y)| HSeg { x0, x1, y })
        .collect();
    let hs = off_text_h(hs, items);
    if hs.len() < 2 {
        return Vec::new();
    }
    let vs: Vec<VSeg> = rules(&bw, false, (h / 120).max(10), thick)
        .into_iter()
        .map(|(y0, y1, x)| VSeg { y0, y1, x })
        .collect();
    let vs = anchored(
        off_text_v(vs, items),
        &cluster(hs.iter().map(|s| s.y).collect(), tol),
        tol * 2.0,
    );
    if vs.len() < 2 {
        return Vec::new();
    }

    // 相交的线段属于同一张表: 并查集分组, 一页上多张表各归各的
    let n = hs.len() + vs.len();
    let mut par: Vec<usize> = (0..n).collect();
    fn find(par: &mut Vec<usize>, mut a: usize) -> usize {
        while par[a] != a {
            par[a] = par[par[a]];
            a = par[a];
        }
        a
    }
    for (i, hseg) in hs.iter().enumerate() {
        for (j, vseg) in vs.iter().enumerate() {
            if hseg.x0 - tol <= vseg.x
                && vseg.x <= hseg.x1 + tol
                && vseg.y0 - tol <= hseg.y
                && hseg.y <= vseg.y1 + tol
            {
                let (ra, rb) = (find(&mut par, i), find(&mut par, hs.len() + j));
                if ra != rb {
                    par[ra] = rb;
                }
            }
        }
    }
    let mut groups: std::collections::HashMap<usize, (Vec<HSeg>, Vec<VSeg>)> = Default::default();
    for (i, s) in hs.iter().enumerate() {
        let r = find(&mut par, i);
        groups.entry(r).or_default().0.push(*s);
    }
    for (j, s) in vs.iter().enumerate() {
        let r = find(&mut par, hs.len() + j);
        groups.entry(r).or_default().1.push(*s);
    }

    let mut grids = Vec::new();
    for (gh, gv) in groups.into_values() {
        if gh.len() < 2 || gv.len() < 2 {
            continue;
        }
        let ys = cluster(gh.iter().map(|s| s.y).collect(), tol);
        let xs = cluster(gv.iter().map(|s| s.x).collect(), tol);
        // 太窄太扁的多半是签名栏下划线之类, 不当表格
        if ys.len() < 2 || xs.len() < 2 || xs[xs.len() - 1] - xs[0] < 0.15 * w as f32 {
            continue;
        }
        let (cells, merged) = grid_cells(&xs, &ys, &gh, &gv, tol);
        // 只有一个格子的不是表格, 是加了边框的文本框(整页外框也长这样) ——
        // 做成 1x1 表格只会把整段文字塞进一格, 还丢了分段
        if cells.len() < 2 {
            continue;
        }
        grids.push(Grid {
            x0: xs[0],
            y0: ys[0],
            x1: xs[xs.len() - 1],
            y1: ys[ys.len() - 1],
            rxs: xs.iter().map(|x| x / w as f32).collect(),
            xs,
            ys,
            cells,
            merged,
        });
    }
    grids.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap());
    grids
}

/// 一格里的文字接成一段: 上一行排满了就接着写, 没排满的换行
///
/// 格内换行不能一律当续行 —— "手动下单/自动下单"是两条并列的值, 接成
/// 一行就读不出是两项了。
fn join_cell(items: Vec<Box2>, x0: f32, x1: f32, cfg: &Config) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut prev_full = false;
    for ln in super::line::group_lines(items, cfg) {
        let t = super::line::line_text(&ln);
        if t.is_empty() {
            continue;
        }
        if !parts.is_empty() && prev_full {
            let last = parts.last_mut().unwrap();
            let sep = if is_zh(last) { "" } else { " " };
            *last = clean(&format!("{last}{sep}{t}"));
        } else {
            parts.push(t);
        }
        let right = ln.items.iter().map(|i| i.x1).fold(f32::MIN, f32::max);
        prev_full = right - x0 >= 0.85 * (x1 - x0);
    }
    parts.join("\n")
}

/// 把落在网格里的 item 按中心点归进各个(可能合并的)单元格
pub fn fill_grid(g: &mut Grid, items: &[Box2]) {
    let cfg = Config::default();
    // (行,列) -> 归属的单元格下标
    let mut owner = std::collections::HashMap::new();
    for (k, cell) in g.cells.iter().enumerate() {
        for i in cell.r..cell.r + cell.h {
            for j in cell.c..cell.c + cell.w {
                owner.insert((i, j), k);
            }
        }
    }
    let mut bag: std::collections::HashMap<usize, Vec<Box2>> = Default::default();
    for it in items {
        let r = band(&g.ys, (it.y0 + it.y1) / 2.0);
        let c = band(&g.xs, (it.x0 + it.x1) / 2.0);
        if let Some(&k) = owner.get(&(r, c)) {
            bag.entry(k).or_default().push(it.clone());
        }
    }
    for (k, its) in bag {
        let (c0, cw) = (g.cells[k].c, g.cells[k].w);
        g.cells[k].text = join_cell(its, g.xs[c0], g.xs[c0 + cw], &cfg);
    }
}

pub fn in_grid(it: &Box2, grids: &[Grid]) -> bool {
    let cx = (it.x0 + it.x1) / 2.0;
    let cy = (it.y0 + it.y1) / 2.0;
    grids
        .iter()
        .any(|g| g.x0 <= cx && cx <= g.x1 && g.y0 <= cy && cy <= g.y1)
}
