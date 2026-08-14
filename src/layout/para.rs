//! 单列块合并续行 + 表格区按列归位

use regex::Regex;
use std::sync::OnceLock;

use super::block::col_of;
use super::line::line_text;
use super::{clean, is_zh, Line};
use crate::config::Config;

macro_rules! lazy_re {
    ($name:ident, $pat:expr) => {
        pub fn $name() -> &'static Regex {
            static P: OnceLock<Regex> = OnceLock::new();
            P.get_or_init(|| Regex::new($pat).expect(stringify!($name)))
        }
    };
}

lazy_re!(
    num_start,
    r"^(\d{1,2}\.\d{1,2}(\.\d{1,2})?[\s、．.]|\d{1,2}[\.、]\s*|[一二三四五六七八九十]+[、．.]|[（(]\d{1,2}[）)]|[A-Z][\.、]\s|[•·●○\*]\s*|[-–—]\s+)"
);
lazy_re!(end_punct, r#"[。．.：:；;!?！？）)】\]"”]$"#);
lazy_re!(bullet, r"^([•·●○\*]\s*|[-–—]\s+)");

/// 合并续行之后的一段
#[derive(Debug, Clone)]
pub struct Para {
    pub text: String,
    pub rx0: f32,
    pub cx0: f32,
    pub rx1: f32,
    pub ry0: f32,
    /// 居中的短行(多半是大标题)
    pub center: bool,
    pub bullet: bool,
}

/// 把被 OCR 拆散的续行合并回段落; 未排满的行绝不吸收下一行
pub fn merge_paras(lines: &[Line], cfg: &Config) -> Vec<Para> {
    // 本页行距中位数 -> 空行阈值(不同卷宗 1.5% ~ 3.2%, 写死阈值必误伤其一)
    let mut gaps: Vec<f32> = lines
        .windows(2)
        .map(|w| w[1].ry0 - w[0].ry0)
        .filter(|g| *g > 0.0 && *g < 0.08)
        .collect();
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let lead = if gaps.is_empty() {
        0.02
    } else {
        gaps[gaps.len() / 2]
    };
    let gap_max = lead * 1.75;

    let mut paras: Vec<Para> = Vec::new();
    for ln in lines {
        let text = line_text(ln);
        if text.is_empty() {
            continue;
        }
        let w = ln.rx1 - ln.rx0;
        let cur = Para {
            text,
            rx0: ln.rx0,
            cx0: ln.cx0,
            rx1: ln.rx1,
            ry0: ln.ry0,
            center: ((ln.rx0 + ln.rx1) / 2.0 - 0.5).abs() < 0.07 && w < 0.5 && ln.rx0 > 0.25,
            bullet: false,
        };
        let Some(prev) = paras.last_mut() else {
            paras.push(cur);
            continue;
        };
        let new_block = prev.rx1 <= cfg.full_line            // 上一行没排满 -> 它已结束
            || num_start().is_match(&cur.text)               // 新编号/新项目符号
            || end_punct().is_match(&prev.text)              // 上一段已收尾
            || is_zh(&cur.text) != is_zh(&prev.text)         // 中英切换 -> 对照的另一半
            || (cur.cx0 - prev.cx0).abs() > cfg.x_tol        // 缩进层级变了
            || (cur.ry0 - prev.ry0) > gap_max; // 行距明显变大 = 空行
        if new_block {
            paras.push(cur);
        } else {
            let sep = if is_zh(&prev.text) { "" } else { " " };
            prev.text = clean(&format!("{}{sep}{}", prev.text, cur.text));
            prev.rx1 = cur.rx1;
            prev.ry0 = cur.ry0; // 推进基准行, 否则下一续行会被当空行断开
        }
    }
    paras
}

/// OCR 会丢弃 • 图形符号: 用"缩进大于块基准"把列表项找回来
///
/// 基准按块(而非整页)取众数: 技术规格那种整块统一缩进的"标签—值"清单, 块内
/// 众数就是它自己, 于是不会被误判成列表; 换成页级基准反而会把它们全标成 •。
pub fn mark_bullets(mut paras: Vec<Para>, cfg: &Config) -> Vec<Para> {
    if paras.is_empty() {
        return paras;
    }
    let mut cnt: std::collections::HashMap<i64, usize> = Default::default();
    for p in &paras {
        *cnt.entry((p.cx0 * 200.0).round() as i64).or_default() += 1;
    }
    let base = cnt
        .iter()
        .max_by_key(|(_, &n)| n)
        .map(|(&k, _)| k as f32 / 200.0)
        .unwrap_or(0.0);
    for p in paras.iter_mut() {
        // 居中大标题左缩进同样很大, 但不是列表项
        p.bullet = (p.cx0 - base) > cfg.bullet_ind && !num_start().is_match(&p.text) && !p.center;
    }
    paras
}

/// 每行的 item 按列起点归位; 只落在一列里的行是单元格续行, 并回上一行
///
/// "跨两列以上 = 新行"这条判据很关键: "1 | TMS部分,文件 | TMS提供"整行没有序号,
/// 但它确实是新的一行; 而 "含视频线、电话线" 只占描述列一格, 是上一行的续行。
/// 按有没有首列内容来判会把前者判错。
pub fn build_rows(lines: &[Line], starts: &[f32]) -> Vec<Vec<String>> {
    let mut edges: Vec<f32> = starts[1..].to_vec();
    edges.push(1.0);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut ends: Vec<Vec<f32>> = Vec::new(); // 每行各列最后一个 item 的右端
    for ln in lines {
        let mut cells: Vec<Vec<&str>> = vec![Vec::new(); starts.len()];
        let mut right = vec![0.0f32; starts.len()];
        for it in &ln.items {
            let k = col_of(it.rx0, starts);
            cells[k].push(&it.t);
            right[k] = right[k].max(it.rx1);
        }
        let txt: Vec<String> = cells.iter().map(|c| clean(&c.join(" "))).collect();
        let filled: Vec<usize> = txt
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.is_empty())
            .map(|(k, _)| k)
            .collect();
        if filled.is_empty() {
            continue;
        }
        // 表头后面紧跟的单列行不并进表头(它多半是上一页某行的续尾)
        if filled.len() >= 2 || rows.is_empty() || (rows.len() == 1 && header_like(&rows[0])) {
            rows.push(txt);
            ends.push(right);
            continue;
        }
        let k = filled[0];
        let prev = rows.last_mut().unwrap();
        let pend = ends.last_mut().unwrap();
        if prev[k].is_empty() {
            prev[k] = txt[k].clone();
        } else if (pend[k] - starts[k]) / (edges[k] - starts[k]).max(1e-6) >= 0.85 {
            // 上一行把这一列填满了 85% 以上 -> 是折行, 接着写。用填充率而不是
            // "离右边界差多少": 各列宽度相差很大, 绝对值定不出统一阈值
            let sep = if is_zh(&prev[k]) { "" } else { " " };
            prev[k] = clean(&format!("{}{sep}{}", prev[k], txt[k]));
        } else {
            prev[k] = format!("{}\n{}", prev[k], txt[k]); // 没排满 -> 单元格内的另一条
        }
        pend[k] = right[k];
    }
    rows
}

/// 一行是不是"全是短标签"的样子
pub fn header_like(row: &[String]) -> bool {
    let cells: Vec<&String> = row.iter().filter(|c| !c.is_empty()).collect();
    cells.len() >= 2
        && cells
            .iter()
            .all(|c| c.chars().count() <= 12 && !c.contains('\n'))
}

/// 首行全是短标签 -> 表头(加粗 + 跨页重复); 值列很长的"标签—值"清单不算
pub fn is_header_row(rows: &[Vec<String>]) -> bool {
    rows.len() >= 3
        && header_like(&rows[0])
        && !rows[0]
            .iter()
            .any(|c| !c.is_empty() && end_punct().is_match(c))
}

/// 按编号形态判定标题层级; 返回 None 表示正文
pub fn heading_level(text: &str) -> Option<u8> {
    static PATS: OnceLock<Vec<(Regex, u8, usize)>> = OnceLock::new();
    let pats = PATS.get_or_init(|| {
        vec![
            (Regex::new(r"^[A-Z]\.\s+[A-Z]").unwrap(), 1, 70),
            (
                Regex::new(r"^[一二三四五六七八九十]+[、．.]").unwrap(),
                2,
                60,
            ),
            (Regex::new(r"^\d{1,2}\.\d{1,2}[\s、．.]").unwrap(), 3, 90),
            (Regex::new(r"^\d{1,2}\s+[A-Z][a-z]").unwrap(), 3, 70),
        ]
    });
    static SENT: OnceLock<Regex> = OnceLock::new();
    static N2: OnceLock<Regex> = OnceLock::new();
    static NDOT: OnceLock<Regex> = OnceLock::new();
    let t = text.trim();
    if SENT
        .get_or_init(|| Regex::new(r"[。；;]$").unwrap())
        .is_match(t)
    {
        return None; // 完整句子 -> 正文, 不是标题
    }
    if pats[0].0.is_match(t) && t.chars().count() < pats[0].2 {
        return Some(1);
    }
    // "1. 设备" 是二级, 但 "1.1 xxx" 不是
    let n2 = N2.get_or_init(|| Regex::new(r"^\d{1,2}[\.、]\s*\S").unwrap());
    let ndot = NDOT.get_or_init(|| Regex::new(r"^\d{1,2}\.\d").unwrap());
    if n2.is_match(t) && t.chars().count() < 90 && !ndot.is_match(t) {
        return Some(2);
    }
    for (re, lv, maxlen) in pats.iter().skip(1) {
        if re.is_match(t) && t.chars().count() < *maxlen {
            return Some(*lv);
        }
    }
    None
}
