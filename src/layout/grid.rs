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
    /// 四条边在原件上画没画线, 顺序 上 右 下 左
    ///
    /// 不是所有表都四面围严。合同里的签章表常常只有外框没有内线, 报价单里的
    /// 三线表只画上下两条, 还有整张表不画左右边框的 —— 照着原件逐边还原, 比
    /// "要么全画要么全不画"接近得多。
    pub edges: [bool; 4],
}

/// [`Cell::edges`] 的下标
pub const TOP: usize = 0;
pub const RIGHT: usize = 1;
pub const BOTTOM: usize = 2;
pub const LEFT: usize = 3;

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
fn rules(
    bw: &crate::imgutil::Bitmap,
    horiz: bool,
    min_len: usize,
    max_thick: f32,
) -> Vec<(f32, f32, f32)> {
    let opened = bw.open_line(horiz, min_len);
    let mut out = Vec::new();
    for b in connected_components(&opened) {
        let span = if horiz { b.w } else { b.h };
        if span < min_len || b.area as f32 / span as f32 > max_thick {
            continue;
        }
        if horiz {
            out.push((
                b.x as f32,
                (b.x + b.w) as f32,
                b.y as f32 + b.h as f32 / 2.0,
            ));
        } else {
            out.push((
                b.y as f32,
                (b.y + b.h) as f32,
                b.x as f32 + b.w as f32 / 2.0,
            ));
        }
    }
    out
}

/// 把同一条竖线上断开的几截接回去
///
/// 必须在 anchored 之前做。扫描件上一条框线的墨色不匀, 中间淡下去几个像素就
/// 会被二值化切成两截 —— 实测一张手机扫的表格, 右边框在 y=1808 处断了 2 个
/// 像素, 上半截(y 896..1807)下端离横线 22px、下半截(y 1809..1868)上端离横线
/// 24px, 于是两截都过不了 anchored, 整条右边框凭空消失, 表格从两列塌成一列。
///
/// PDF 渲染出来的线是完美连续的, 所以这个坑只在拍照件上踩得到。
///
/// 只接"横向挨得够近(同一条线) 且 纵向缺口比 tol 小"的两截 —— 缺口限制不能
/// 放松, 否则会把上下两张表的边框接成一条。
fn stitch_v(mut vs: Vec<VSeg>, tol: f32) -> Vec<VSeg> {
    // 必须按 y0 排, 不能按 x 排。同一条线的两截 x 会差一点点(墨色不匀,
    // 连通域的中心就偏了), 按 x 排会把上下顺序打乱, 接的时候反而丢掉上半截
    vs.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap());
    let mut out: Vec<VSeg> = Vec::with_capacity(vs.len());
    for v in vs {
        // 找一条 x 对得上、尾巴又刚好接得住的; 找不到就自成一条。
        // 按 y0 排过了, 所以候选的 y0 一定不比 v 大, 只需要看尾巴
        match out
            .iter_mut()
            .find(|p| (p.x - v.x).abs() <= tol && v.y0 - p.y1 <= tol)
        {
            Some(p) => {
                p.y1 = p.y1.max(v.y1);
                // 接完之后 x 取两截的中点, 免得整条线的位置被更短那截带偏
                p.x = (p.x + v.x) / 2.0;
            }
            None => out.push(v),
        }
    }
    out
}

/// 只留横跨了两条以上横线的竖线
///
/// 汉字的竖笔跟框线一样细一样直, 长度也能碰到阈值; 区别是笔画悬在格子中间,
/// 上下都不碰横线。不加这条, 一页正文能"检出"三百多条竖线。
///
/// 判据是"身上压过几条横线", 不是"两端离横线多近"。框线画到拐角常常还要探出
/// 去一截: 实测 配套.pdf 第 4 页那张签章表, 左边框 y=993..1712, 下横线在
/// 1699 —— 探出 13px, 比原来 12px 的容差多 1 个像素, 整条左边框就没了, 表从
/// 两列塌成一列。按"压过几条"算, 探出多少都不影响。
///
/// 顺带还更严了一点: 原来两端都贴着"同一条"横线的极短一截也算数, 现在得压过
/// 两条不同的才算。
fn anchored(vs: Vec<VSeg>, ys: &[f32], tol: f32) -> Vec<VSeg> {
    vs.into_iter()
        .filter(|v| {
            ys.iter()
                .filter(|&&y| v.y0 - tol <= y && y <= v.y1 + tol)
                .count()
                >= 2
        })
        .collect()
}

/// 线要落在文字框的"内部"才算笔画 —— 贴着框边走的是框线
///
/// 判据不能写成"±2px 的包含"。OCR 的检测框会把紧挨正文的那条框线一起框进去,
/// 框的边正好压在框线上: 实测 配套.pdf 第 4 页, 上一段正文的框是 y=925..986,
/// 表格上边框就在 y=986, 覆盖率 0.99 —— 按容差判, 整条上边框被当成笔画扔掉,
/// 那张表只剩下边框一条横线, gh<2 于是降级成无框线表, 六条边写成 none。
/// "内容还在, 表格线没了"就是这么来的。
///
/// 缩进取框厚的 15%(至少 2px): 笔画长在字身中间, 缩这么点滤不掉; 框线要么在
/// 框外, 要么正压在框边上, 一缩就露出来。
fn inside(lo: f32, hi: f32, v: f32) -> bool {
    let m = ((hi - lo) * 0.15).max(2.0);
    lo + m <= v && v <= hi - m
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
                if inside(it.y0, it.y1, s.y) {
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
                if inside(it.x0, it.x1, s.x) {
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

/// 这块地方有没有字 —— 补表格边界时用来分辨"那儿真有一列"和"线探出去一截"
fn has_text(items: &[Box2], x0: f32, x1: f32, y0: f32, y1: f32) -> bool {
    items.iter().any(|it| {
        let (cx, cy) = ((it.x0 + it.x1) / 2.0, (it.y0 + it.y1) / 2.0);
        x0 <= cx && cx <= x1 && y0 <= cy && cy <= y1
    })
}

/// v 落在 edges 划出的第几格
fn band(edges: &[f32], v: f32) -> usize {
    // 最后一条边是右/下边界, 不单独成格, 所以只在前 n-1 条里找
    let inner = edges.len().saturating_sub(1);
    edges[..inner].iter().rposition(|&e| v >= e).unwrap_or(0)
}

/// 网格里哪些格是连在一起的: 缺了内部框线就是原件里的合并单元格
fn grid_cells(xs: &[f32], ys: &[f32], hs: &[HSeg], vs: &[VSeg], tol: f32) -> (Vec<Cell>, usize) {
    let (nr, nc) = (ys.len() - 1, xs.len() - 1);
    // 第 j 条竖线在 r0..r1 这几行上画没画(要盖住六成才算)
    let vline = |j: usize, r0: usize, r1: usize| -> bool {
        let (y0, y1) = (ys[r0], ys[r1]);
        vs.iter()
            .any(|v| (v.x - xs[j]).abs() <= tol && v.y1.min(y1) - v.y0.max(y0) >= 0.6 * (y1 - y0))
    };
    // 第 i 条横线在 c0..c1 这几列上画没画
    let hline = |i: usize, c0: usize, c1: usize| -> bool {
        let (x0, x1) = (xs[c0], xs[c1]);
        hs.iter()
            .any(|h| (h.y - ys[i]).abs() <= tol && h.x1.min(x1) - h.x0.max(x0) >= 0.6 * (x1 - x0))
    };
    // (r,c) 右边有没有竖线
    let has_v = |r: usize, c: usize| vline(c + 1, r, r + 1);
    // (r,c) 下边有没有横线
    let has_h = |r: usize, c: usize| hline(r + 1, c, c + 1);
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
            while r + h < nr && !used[r + h][c] && (0..w).all(|k| !has_h(r + h - 1, c + k)) {
                h += 1;
            }
            for row in &mut used[r..r + h] {
                row[c..c + w].fill(true);
            }
            let mut edges = [false; 4];
            edges[TOP] = hline(r, c, c + w);
            edges[BOTTOM] = hline(r + h, c, c + w);
            edges[LEFT] = vline(c, r, r + h);
            edges[RIGHT] = vline(c + w, r, r + h);
            cells.push(Cell {
                r,
                c,
                h,
                w,
                text: String::new(),
                edges,
            });
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
        stitch_v(off_text_v(vs, items), tol),
        &cluster(hs.iter().map(|s| s.y).collect(), tol),
        tol * 2.0,
    );
    if vs.len() < 2 {
        return Vec::new();
    }

    // 相交的线段属于同一张表: 并查集分组, 一页上多张表各归各的
    let n = hs.len() + vs.len();
    let mut par: Vec<usize> = (0..n).collect();
    fn find(par: &mut [usize], mut a: usize) -> usize {
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

    // 按并查集的根排一下再遍历: 根本身是确定的, 但 HashMap 的遍历顺序不是,
    // 直接 into_values() 会让多张表的先后顺序每次运行都不一样
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by_key(|&(root, _)| root);

    let mut grids = Vec::new();
    for (_, (gh, gv)) in groups {
        if gh.len() < 2 || gv.len() < 2 {
            continue;
        }
        let mut ys = cluster(gh.iter().map(|s| s.y).collect(), tol);
        let mut xs = cluster(gv.iter().map(|s| s.x).collect(), tol);

        // 不是每张表都四面围严: 左右不封口的、只画上下两条的三线表, 都常见。
        // 这时列边界只到最外侧那条内部竖线, 外面那一列连字带线全掉在表外 ——
        // 横线画到哪儿表就宽到哪儿, 拿它把边界补回来, 竖线补上下同理。
        // 只在补出来的那一条里确实有字时才补, 免得线探出去一截就凭空多一空列。
        let hx0 = gh.iter().fold(f32::MAX, |a, s| a.min(s.x0));
        let hx1 = gh.iter().fold(f32::MIN, |a, s| a.max(s.x1));
        let (ty, by) = (ys[0], ys[ys.len() - 1]);
        if hx0 < xs[0] - tol && has_text(items, hx0, xs[0], ty, by) {
            xs.insert(0, hx0);
        }
        if hx1 > xs[xs.len() - 1] + tol && has_text(items, xs[xs.len() - 1], hx1, ty, by) {
            xs.push(hx1);
        }
        let vy0 = gv.iter().fold(f32::MAX, |a, s| a.min(s.y0));
        let vy1 = gv.iter().fold(f32::MIN, |a, s| a.max(s.y1));
        let (lx, rx) = (xs[0], xs[xs.len() - 1]);
        if vy0 < ys[0] - tol && has_text(items, lx, rx, vy0, ys[0]) {
            ys.insert(0, vy0);
        }
        if vy1 > ys[ys.len() - 1] + tol && has_text(items, lx, rx, ys[ys.len() - 1], vy1) {
            ys.push(vy1);
        }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn v(y0: f32, y1: f32, x: f32) -> VSeg {
        VSeg { y0, y1, x }
    }

    /// 手机扫的表格上, 右边框在 y=1808 断了 2 个像素。下半截压不到两条横线,
    /// 到不了最后一行, 表格末行的右边框就没了。数字取自那张真实样张。
    #[test]
    fn stitch_rescues_a_broken_border() {
        let tol = 1756.0 / 300.0; // 5.85
        let ys = [903.0, 981.0, 1785.0, 1864.0];
        let broken = vec![v(896.0, 1807.0, 1489.0), v(1809.0, 1868.0, 1490.0)];

        let raw = anchored(broken.clone(), &ys, tol * 2.0);
        assert_eq!(raw.len(), 1, "没接之前只剩上半截");
        assert!(raw[0].y1 < 1864.0, "上半截够不着最后一条横线");

        let whole = stitch_v(broken, tol);
        assert_eq!(whole.len(), 1, "两截该接成一条");
        let ok = anchored(whole, &ys, tol * 2.0);
        assert_eq!(ok.len(), 1, "接完该能锚上");
        assert!(ok[0].y1 > 1864.0, "接完才盖得住整条右边框");
    }

    /// 框线画到拐角常常探出去一截。实测 配套.pdf 第 4 页那张签章表, 左边框
    /// y=993..1712, 上下横线在 986 / 1699 —— 下端探出 13px。按"两端离横线多近"
    /// 判, 12px 的容差差 1 个像素就把整条左边框判没了, 表从两列塌成一列。
    #[test]
    fn keeps_a_border_that_overshoots_the_corner() {
        let ys = [986.0, 1699.0];
        let left = vec![v(993.0, 1712.0, 176.0)];
        assert_eq!(anchored(left, &ys, 12.0).len(), 1, "探出去一截照样是边框");
    }

    /// 但悬在格子中间的竖笔还是得滤掉 —— 一条横线都压不着
    #[test]
    fn still_drops_a_stroke_floating_inside_a_cell() {
        let ys = [986.0, 1699.0];
        let stroke = vec![v(1180.0, 1240.0, 408.0)];
        assert!(anchored(stroke, &ys, 12.0).is_empty());
    }

    /// 缺口大的不能接 —— 上下两张表的边框接成一条会更糟
    #[test]
    fn stitch_leaves_far_apart_segments_alone() {
        let tol = 5.85;
        let far = vec![v(100.0, 200.0, 500.0), v(400.0, 500.0, 500.0)];
        assert_eq!(stitch_v(far, tol).len(), 2);
    }

    /// 横向差太远的是两条不同的竖线, 不能因为纵向接得上就并了
    #[test]
    fn stitch_keeps_separate_columns_apart() {
        let tol = 5.85;
        let two = vec![v(100.0, 200.0, 500.0), v(201.0, 300.0, 900.0)];
        assert_eq!(stitch_v(two, tol).len(), 2);
    }

    fn h(x0: f32, x1: f32, y: f32) -> HSeg {
        HSeg { x0, x1, y }
    }

    fn bx(x0: f32, y0: f32, x1: f32, y1: f32) -> Box2 {
        Box2 {
            t: "x".into(),
            x0,
            y0,
            x1,
            y1,
            s: 0.9,
            rx0: 0.0,
            rx1: 0.0,
            ry0: 0.0,
            ry1: 0.0,
        }
    }

    /// 紧挨正文的那条表格上边框, 会被 OCR 检测框的底边正好压住。
    ///
    /// 数字取自 配套.pdf 第 4 页: 上一段正文的框 y=925..986, 表格上边框在
    /// y=986, x 方向盖了 0.99。按"±2px 的包含"判, 这条边框被当成笔画扔掉,
    /// 那张签章表只剩下边框一条横线, 降级成无框线表, 六条边写成 none ——
    /// 用户看到的就是"内容还在, 表格线丢了"。
    #[test]
    fn keeps_a_rule_flush_with_the_text_box_below_it() {
        let para = bx(161.0, 925.0, 1615.0, 986.0);
        let top_rule = h(170.0, 1632.0, 986.0);
        assert_eq!(
            off_text_h(vec![top_rule], &[para]).len(),
            1,
            "贴着正文框底边的这条是表格上边框, 不能当笔画扔"
        );
    }

    /// 但真笔画还是得滤掉, 否则一页正文能"检出"几十条横线。
    /// 同样取自那一页: 一个 y=1002..1049 的文字框里, 有条 x=380..438 的横笔。
    #[test]
    fn still_drops_a_stroke_inside_the_text_box() {
        let line = bx(184.0, 1002.0, 664.0, 1049.0);
        let stroke = h(380.0, 438.0, 1018.0);
        assert!(off_text_h(vec![stroke], &[line]).is_empty(), "这是笔画");
    }

    /// 只有外框没有内线的签章表: 四周画满, 中缝那条竖线两边也画,
    /// 而两个格子之间没有横线可分 —— 逐边记下来才还得回原样
    #[test]
    fn records_which_edges_were_actually_drawn() {
        let xs = [176.0, 922.0, 1634.0];
        let ys = [986.0, 1699.0];
        let hs = [h(170.0, 1632.0, 986.0), h(178.0, 1640.0, 1699.0)];
        let vs = [
            v(993.0, 1712.0, 176.0),
            v(984.0, 1700.0, 922.0),
            v(975.0, 1690.0, 1634.0),
        ];
        let (cells, merged) = grid_cells(&xs, &ys, &hs, &vs, 6.0);
        assert_eq!((cells.len(), merged), (2, 0), "一行两列, 没有合并");
        for c in &cells {
            assert_eq!(c.edges, [true; 4], "这张表四边都画了");
        }
    }

    /// 三线表: 只有上下两条横线, 左右不封口, 中间一条竖线分栏。
    /// 左右两条边不能凭空补出来。
    #[test]
    fn leaves_unruled_edges_unruled() {
        let xs = [100.0, 500.0, 900.0];
        let ys = [100.0, 300.0];
        let hs = [h(100.0, 900.0, 100.0), h(100.0, 900.0, 300.0)];
        let vs = [v(100.0, 300.0, 500.0)];
        let (cells, _) = grid_cells(&xs, &ys, &hs, &vs, 6.0);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].edges, [true, true, true, false], "左边没线");
        assert_eq!(cells[1].edges, [true, false, true, true], "右边没线");
    }
}
