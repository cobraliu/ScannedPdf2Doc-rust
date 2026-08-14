//! 块划分: 表格区 vs 正文区

use super::grid::Grid;
use super::{Block, Line};
use crate::config::Config;

/// 一行内部最大的横向空白: 表格行的列间距远大于正文里的词间距
fn max_gap(ln: &Line) -> f32 {
    ln.items
        .windows(2)
        .map(|w| w[1].rx0 - w[0].rx1)
        .fold(0.0f32, f32::max)
}

/// 行内 item 合成横向区间: 横向叠掉一半以上的两个 item 是被并成一行的
/// 上下两行(表头"不含税单价"压着"(元)"那种), 属于同一格, 不能各算一列。
///
/// 只是首尾相接不算 —— 序号列收在 0.173、数量列正好起于 0.173, 并掉的话
/// 整个数量列就没了。
fn spans(ln: &Line) -> Vec<(f32, f32)> {
    let mut its: Vec<&super::Box2> = ln.items.iter().collect();
    its.sort_by(|a, b| a.rx0.partial_cmp(&b.rx0).unwrap());
    let mut out: Vec<(f32, f32)> = Vec::new();
    for it in its {
        let (a, b) = (it.rx0, it.rx1);
        let ov = match out.last() {
            Some(&(_, hi)) => hi.min(b) - a,
            None => 0.0,
        };
        match out.last_mut() {
            Some(last) if ov > 0.0 && ov >= 0.5 * (b - a).min(last.1 - last.0) => {
                last.1 = last.1.max(b);
            }
            _ => out.push((a, b)),
        }
    }
    out
}

const COL_TOL: f32 = 0.022;

/// 列起点 = 各格左边界的聚类中心, 行数够时要求至少两行对齐才算一列
///
/// 不用"找贯穿空白通道"那套: 扫描件里相邻单元格常常几乎贴着(实测描述列收在
/// 0.775, 备注列起于 0.790, 只差 1.5%), 通道法会把这两列并掉; 左边界对齐则稳。
fn col_starts(lines: &[Line]) -> Vec<f32> {
    let mut sp: Vec<(f32, f32, usize)> = Vec::new();
    for (k, ln) in lines.iter().enumerate() {
        for (a, b) in spans(ln) {
            sp.push((a, b, k));
        }
    }
    if sp.is_empty() {
        return Vec::new();
    }
    sp.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());

    // 格内折行往往比首行缩进几个字, 左边界会另立一列。它整个被同列另一格包着,
    // 就归到那一格的起点上去。另一套版面的行跟表格行只是错开、互不包含, 归不掉
    // —— 后面由 crosses 把它分出表格区。
    let mut xs: Vec<f32> = Vec::with_capacity(sp.len());
    for &(a, b, k) in &sp {
        let host = sp
            .iter()
            .filter(|&&(aa, bb, kk)| kk != k && aa <= a - COL_TOL && bb >= b - 0.01)
            .map(|&(aa, _, _)| aa)
            .fold(f32::MAX, f32::min);
        xs.push(if host == f32::MAX { a } else { host });
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());

    let need = if lines.len() > 1 { 2 } else { 1 };
    let mut groups: Vec<Vec<f32>> = vec![vec![xs[0]]];
    for &x in &xs[1..] {
        let last = *groups.last().unwrap().last().unwrap();
        if x - last <= COL_TOL {
            groups.last_mut().unwrap().push(x);
        } else {
            groups.push(vec![x]);
        }
    }
    let mut starts: Vec<f32> = groups
        .iter()
        .filter(|g| g.len() >= need)
        .map(|g| g.iter().sum::<f32>() / g.len() as f32)
        .collect();
    // 最左一列常常只有一行有内容(表头缩进、序号列多半空着), 聚类会把它丢掉;
    // 丢了的话 col_of 会把左边的字全塞进第二列, 所以补回来
    if !starts.is_empty() && xs[0] < starts[0] - COL_TOL {
        starts.insert(0, xs[0]);
    }
    starts
}

/// 行内有没有文字横跨列边界 —— 真表格的文字不越格, 整幅宽的散文才越
fn crosses(ln: &Line, starts: &[f32]) -> bool {
    spans(ln)
        .iter()
        .any(|&(a, b)| starts.iter().any(|&s| a < s - 0.02 && b > s + 0.02))
}

/// 这组行能不能共用一套列: 能则给出列起点, 不能返回 None
fn fit(rows: &[Line]) -> Option<Vec<f32>> {
    let starts = col_starts(rows);
    if starts.len() < 2 {
        return None;
    }
    if rows.iter().any(|ln| crosses(ln, &starts)) {
        None
    } else {
        Some(starts)
    }
}

/// item 落在哪一列: 取不超过它左边界的最后一个列起点
pub fn col_of(x: f32, starts: &[f32]) -> usize {
    let mut k = 0;
    for (i, &s) in starts.iter().enumerate() {
        if x >= s - 0.02 {
            k = i;
        }
    }
    k
}

/// 从第 i 行(一个 seed)往下扩表格区, 返回 (末行下标, 列起点, seed 行数)
///
/// 往下并一个 seed 之前先试算: 加进来之后整组还得共用同一套列。列位对不上的
/// seed 就是另一回事了, 到此为止 —— 合同抬头"需方：XX ␣␣ 签订时间：YY"跟下面的
/// 产品明细表列位完全不同, 靠这条才不会被并成一张列数虚高、大半格是空的表。
/// 夹在中间的单列行是格内续行, 一并收下; 越格的(整幅宽的散文)截断。
fn grow(lines: &[Line], seeds: &[bool], i: usize) -> (usize, Option<Vec<f32>>, usize) {
    let n = lines.len();
    let mut cols = col_starts(&lines[i..i + 1]);
    let mut cur: Vec<Line> = vec![lines[i].clone()];
    let mut end = i;
    let mut j = i + 1;
    while j < n {
        if lines[j].ry0 - lines[j - 1].ry0 >= 0.05 {
            break; // 行距明显拉开 -> 不是同一张表
        }
        if seeds[j] {
            let mut trial_rows = cur.clone();
            trial_rows.push(lines[j].clone());
            match fit(&trial_rows) {
                None => break,
                Some(t) => {
                    cols = t;
                    cur = trial_rows;
                    end = j;
                }
            }
        } else if !cols.is_empty() && crosses(&lines[j], &cols) {
            break;
        }
        j += 1;
    }
    let c = if cols.len() >= 2 { Some(cols) } else { None };
    (end, c, cur.len())
}

/// 切成 [表格块, 正文块, ...]
///
/// 表格行(seed)的判据是"行内有超过 gutter 的横向空白"; 表格区怎么长见 grow。
/// 表格尾部之后的正文行不收 —— 所以按"最后一个 seed"截断。
pub fn split_blocks(lines: &[Line], cfg: &Config) -> Vec<Block> {
    let seeds: Vec<bool> = lines.iter().map(|ln| max_gap(ln) >= cfg.gutter).collect();
    let n = lines.len();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < n {
        if !seeds[i] {
            let mut j = i;
            while j < n && !seeds[j] {
                j += 1;
            }
            blocks.push(Block::Text(lines[i..j].to_vec()));
            i = j;
            continue;
        }
        let (end, cols, rows) = grow(lines, &seeds, i);
        let seg = lines[i..=end].to_vec();
        match cols {
            Some(c) if cfg.tables && rows >= cfg.min_tbl_rows => blocks.push(Block::Table(seg, c)),
            _ => blocks.push(Block::Text(seg)),
        }
        i = end + 1;
    }
    blocks
}

/// 按 y 在框线表处把正文行切开, 再各自分块 -> 网格与正文保持原来的先后
///
/// 不能先整页分块再把网格插进去: 表格里的字被网格拿走后, 表格上下的正文
/// 变成连续的一整块, 网格无论排在它前面还是后面都是错的。
pub fn weave(lines: &[Line], grids: &[Grid], cfg: &Config) -> Vec<Block> {
    if grids.is_empty() {
        return split_blocks(lines, cfg);
    }
    let mut out = Vec::new();
    let mut cur: Vec<Line> = Vec::new();
    let mut gi = 0usize;
    for ln in lines {
        // group_lines 已按 y 排好
        while gi < grids.len() && ln.y0 > grids[gi].y1 {
            out.extend(split_blocks(&cur, cfg));
            out.push(Block::Grid(grids[gi].clone()));
            cur.clear();
            gi += 1;
        }
        cur.push(ln.clone());
    }
    out.extend(split_blocks(&cur, cfg));
    for g in &grids[gi..] {
        out.push(Block::Grid(g.clone()));
    }
    out
}
