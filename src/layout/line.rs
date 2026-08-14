//! 视觉行聚类 + 分式识别

use super::{clean, Box2, Line};
use crate::config::Config;
use crate::imgutil::Gray;

/// 私有区字符: 分式占位, 形如 SEP 分子 SEP 分母 SEP
pub const FRAC_SEP: char = '\u{e000}';

/// y 区间重叠超过 line_tol 的 item 合并为一个视觉行
///
/// 行包络会随并入的 item 往下长, 这是有意的: 双行表头里"产品名称"这种单行
/// 标签在格内垂直居中, 中心跟下面那行对齐, 只有让包络吃下整个表头才并得对。
/// 代价是包络可能被一个跨两行的噪声 item 搭桥、一路长下去 —— 那条路由
/// drop_noise 剔页边碎片堵住, 不在这里收紧。
pub fn group_lines(mut items: Vec<Box2>, cfg: &Config) -> Vec<Line> {
    items.sort_by(|a, b| {
        a.y0.partial_cmp(&b.y0)
            .unwrap()
            .then(a.x0.partial_cmp(&b.x0).unwrap())
    });
    let mut lines: Vec<Line> = Vec::new();
    for it in items {
        let mut placed = false;
        for ln in lines.iter_mut() {
            let ov = ln.y1.min(it.y1) - ln.y0.max(it.y0);
            let h = (ln.y1 - ln.y0).min(it.y1 - it.y0);
            if h > 0.0 && ov / h > cfg.line_tol {
                ln.y0 = ln.y0.min(it.y0);
                ln.y1 = ln.y1.max(it.y1);
                ln.items.push(it.clone());
                placed = true;
                break;
            }
        }
        if !placed {
            lines.push(Line {
                y0: it.y0,
                y1: it.y1,
                items: vec![it],
                rx0: 0.0,
                rx1: 0.0,
                ry0: 0.0,
                h: 0.0,
                cx0: 0.0,
            });
        }
    }
    for ln in lines.iter_mut() {
        ln.refresh();
    }
    lines.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap());
    lines
}

/// a、b 是不是上下叠排的一对(横向叠过半 + 一个明显在另一个上方)
fn stacked(a: &Box2, b: &Box2) -> Option<(usize, usize)> {
    let ov = a.rx1.min(b.rx1) - a.rx0.max(b.rx0);
    if ov <= 0.5 * (a.rx1 - a.rx0).min(b.rx1 - b.rx0) {
        return None;
    }
    let (up, lo) = if a.y0 < b.y0 { (a, b) } else { (b, a) };
    if lo.y0 < up.y0 + 0.5 * (up.y1 - up.y0) {
        return None;
    }
    Some((0, 0)) // 占位, 真正的下标由调用方给
}

/// 两个 item 之间有没有一条分数线: 又细又孤立的横向墨迹
///
/// 光靠"这一行墨迹多"不行 —— 密排汉字行本身就能占 58%, 表格框线更是满格。
/// 分数线的签名是: 峰值覆盖 >60%、连续高覆盖不超过 5px、上下 3-4px 处几乎全白。
fn has_bar(img: &Gray, up: &Box2, lo: &Box2) -> bool {
    let x0 = up.x0.min(lo.x0).max(0.0) as usize;
    let x1 = (up.x1.max(lo.x1) as usize).min(img.w);
    let y0 = (up.y1 - 0.35 * (up.y1 - up.y0)).max(0.0) as usize;
    let y1 = ((lo.y0 + 0.35 * (lo.y1 - lo.y0)) as usize).min(img.h);
    if x1 <= x0 || y1 <= y0 {
        return false;
    }
    let w = x1 - x0;
    if w < 10 || y1 - y0 < 3 {
        return false;
    }
    let cov: Vec<f32> = (y0..y1)
        .map(|y| (x0..x1).filter(|&x| img.at(x, y) < 160).count() as f32 / w as f32)
        .collect();
    let mut k = 0usize;
    for (i, c) in cov.iter().enumerate() {
        if *c > cov[k] {
            k = i;
        }
    }
    if cov[k] <= 0.6 || cov.iter().filter(|c| **c > 0.6).count() > 5 {
        return false;
    }
    let mut halo = 0.0f32;
    for d in [-4i32, -3, 3, 4] {
        let i = k as i32 + d;
        if i >= 0 && (i as usize) < cov.len() {
            halo = halo.max(cov[i as usize]);
        }
    }
    halo < 0.3
}

/// 把"上下叠排 + 中间一条分数线"的两个 item 合成一个分式 item
///
/// 合成后它就是普通的一格, 后面的列判定/续行合并都不用管分式这回事;
/// 真正的上下叠排留到出 docx 时由 OMML 还原。
pub fn find_fracs(mut lines: Vec<Line>, img: &Gray) -> Vec<Line> {
    for ln in lines.iter_mut() {
        loop {
            let mut hit: Option<(usize, usize)> = None;
            'outer: for i in 0..ln.items.len() {
                for j in (i + 1)..ln.items.len() {
                    if stacked(&ln.items[i], &ln.items[j]).is_none() {
                        continue;
                    }
                    let (a, b) = (&ln.items[i], &ln.items[j]);
                    let (ui, li) = if a.y0 < b.y0 { (i, j) } else { (j, i) };
                    if has_bar(img, &ln.items[ui], &ln.items[li]) {
                        hit = Some((ui, li));
                        break 'outer;
                    }
                }
            }
            let Some((ui, li)) = hit else { break };
            let up = ln.items[ui].clone();
            let lo = ln.items[li].clone();
            let merged = Box2 {
                t: format!("{FRAC_SEP}{}{FRAC_SEP}{}{FRAC_SEP}", up.t, lo.t),
                x0: up.x0.min(lo.x0),
                x1: up.x1.max(lo.x1),
                y0: up.y0,
                y1: lo.y1,
                rx0: up.rx0.min(lo.rx0),
                rx1: up.rx1.max(lo.rx1),
                ry0: up.ry0,
                ry1: lo.ry1,
                s: up.s.min(lo.s),
            };
            let (hi, low) = (ui.max(li), ui.min(li));
            ln.items.remove(hi);
            ln.items.remove(low);
            ln.items.push(merged);
            ln.refresh();
        }
    }
    lines
}

/// 一行的文字拼起来
pub fn line_text(ln: &Line) -> String {
    clean(
        &ln.items
            .iter()
            .map(|i| i.t.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    )
}
