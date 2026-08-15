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

// 字母序号大小写都要认, 小写罗马数字也是一档。Word 默认的多级列表就是
// "1. / a. / i."(再往下 (1) / (a)), 只认 [A-Z] 的话, 从第二级起每一条都会被
// 当成上一条的续行粘上去 —— 一整页列表能糊成两三段。
//
// 点后面写成 `\s|[A-Z][a-z]` 而不是光一个 \s, 是因为识别偶尔不吐那个空格,
// 同一页里 "a. Unified" 有空格、"b.Because" 就没有。放宽的同时得挡住缩写:
//   e.g. / i.e.  点后是小写      -> 不匹配
//   P.R. China / U.S. Gov  点后是大写但紧跟着又一个点 -> [a-z] 卡住, 不匹配
// 也就是"点后面得跟一个真正的词", 而不是另一截缩写。P.R. China 这行在
// 3#线.pdf 里就是一条续行, 误判成列表项会把地址拆成两段。
lazy_re!(
    num_start,
    r"^(\d{1,2}\.\d{1,2}(\.\d{1,2})?[\s、．.]|\d{1,2}[\.、]\s*|[一二三四五六七八九十]+[、．.]|[（(]\d{1,2}[）)]|[（(][A-Za-z][）)]|[ivxIVX]{2,4}[\.、](\s|[A-Z][a-z])|[A-Za-z][\.、](\s|[A-Z][a-z])|[•·●○\*]\s*|[-–—]\s+)"
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
        let cur_marker = num_start().is_match(&cur.text);
        // 悬挂缩进: 列表项的续行对齐到序号后面的正文, 比首行更靠右
        //
        //   a. VIP data is used for matching, but cannot provide data to
        //      cannot obtain (Possible solution: ...)
        //
        // 首行 cx0 落在序号上(实测 0.079), 续行落在正文上(0.107), 差的正是
        // "a. " 那点宽度。这个差常常压过 x_tol(0.025), 于是"缩进层级变了"把每条
        // 列表项都从中间劈成两段 —— 一页四级嵌套列表能劈出二十来段, 看上去就是
        // 版式全乱。
        //
        // 只在"上一段是列表项、这一行自己没有序号"时放行: 真正的下一级子项必然
        // 自带序号, 上面 num_start 那条已经先把它断开了。右移超过约三个字符就
        // 不再当悬挂缩进, 那是另起一栏; 左移一律照旧断开(那是退回外层)。
        let hang = !cur_marker
            && num_start().is_match(&prev.text)
            && (0.0..=0.06).contains(&(cur.cx0 - prev.cx0));
        let new_block = prev.rx1 <= cfg.full_line            // 上一行没排满 -> 它已结束
            || cur_marker                                    // 新编号/新项目符号
            || end_punct().is_match(&prev.text)              // 上一段已收尾
            || is_zh(&cur.text) != is_zh(&prev.text)         // 中英切换 -> 对照的另一半
            || (!hang && (cur.cx0 - prev.cx0).abs() > cfg.x_tol) // 缩进层级变了
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
    // 打平时取缩进最小的那个, 不能只写 max_by_key(n)
    //
    // HashMap 每个进程的哈希种子都不一样, 遍历顺序跟着变。只按 n 取最大, 一旦
    // 两个缩进出现次数相同, 返回哪个纯看运气 —— 同一个二进制、同一张图, 这一页
    // 就会一会儿是缩进段一会儿是项目符号列表。Reverse(k) 把平局定死在最小缩进上,
    // 跟 examples/sample_scanned.docx 那份基准对得上。
    let base = cnt
        .iter()
        .max_by_key(|&(&k, &n)| (n, std::cmp::Reverse(k)))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Box2;

    /// 一行只放一个 item, 行宽/缩进直接给死 —— 这些测试只关心断段判据
    fn line(t: &str, rx0: f32, rx1: f32, ry0: f32) -> Line {
        let b = Box2 {
            t: t.into(),
            x0: rx0,
            y0: ry0,
            x1: rx1,
            y1: ry0 + 0.012,
            s: 1.0,
            rx0,
            rx1,
            ry0,
            ry1: ry0 + 0.012,
        };
        Line {
            y0: ry0,
            y1: ry0 + 0.012,
            items: vec![b],
            rx0,
            rx1,
            ry0,
            h: 0.012,
            cx0: rx0,
        }
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        merge_paras(lines, &Config::default())
            .into_iter()
            .map(|p| p.text)
            .collect()
    }

    /// 小写字母序号的列表项被当成上一项的续行, 两条粘成一段
    ///
    /// 取自 Conclusion-for-QA 那份样张: a. 那行几乎排满(rx1=0.93 > full_line),
    /// 不以标点收尾, 跟 b. 缩进相同、行距正常 —— 于是六条判据里只剩"新编号"
    /// 能救, 而 num_start 当时只认大写 [A-Z], 小写一条都不认。
    #[test]
    fn lowercase_letter_marker_starts_a_new_para() {
        let ls = [
            line("a. Unified DataFeed module, using UDP multicast and other forms for data distribution",
                 0.14, 0.93, 0.30),
            line("b. Because the Update Interval of the OrderBook obtained by live trading reaches 50ms, the",
                 0.14, 0.93, 0.32),
        ];
        assert_eq!(texts(&ls).len(), 2, "b. 是新的一条, 不是 a. 的续行");
    }

    /// 小写罗马数字同理: i. / ii. / iii.
    #[test]
    fn lowercase_roman_marker_starts_a_new_para() {
        let ls = [
            line("i. Idempotence class operations can be issued directly at the same time",
                 0.18, 0.90, 0.30),
            line("ii. Non-idempotence operations select the optimal endpoint based on certain request",
                 0.18, 0.90, 0.32),
        ];
        assert_eq!(texts(&ls).len(), 2, "ii. 是新的一条, 不是 i. 的续行");
    }

    /// 真续行还得照并 —— 别为了断开列表项把正常的折行也拆了
    #[test]
    fn plain_wrapped_line_still_merges() {
        let ls = [
            line("a. Add a layer of spooler/aggregator processing between Execution Engine and Exchange for",
                 0.14, 0.93, 0.30),
            line("order consolidation and internal transactions, reducing transaction requests with",
                 0.14, 0.88, 0.32),
        ];
        assert_eq!(texts(&ls).len(), 1, "这是折行, 该并回同一段");
    }

    /// 识别偶尔不吐序号后那个空格, 同一页里 a. 有、b. 没有
    #[test]
    fn marker_without_space_after_dot_still_counts() {
        let ls = [
            line("a. Unified DataFeed module, using UDP multicast and other forms for data distribution",
                 0.14, 0.93, 0.30),
            line("b.Because the Update Interval of the OrderBook obtained by live trading reaches 50ms, the",
                 0.14, 0.93, 0.32),
        ];
        assert_eq!(texts(&ls).len(), 2, "b.Because 没空格, 也还是新的一条");
    }

    /// 缩写不能误伤: "e.g." 点后是小写
    #[test]
    fn abbreviation_is_not_a_marker() {
        assert!(!num_start().is_match("e.g. the first case"));
        assert!(!num_start().is_match("i.e. the second case"));
    }

    /// 缩写里点后面是大写也不行 —— 得是"点后跟一个真正的词"
    ///
    /// "P.R. China" 在 3#线.pdf 里是地址的续行, 判成列表项会把地址拆成两段
    #[test]
    fn initialism_is_not_a_marker() {
        assert!(!num_start().is_match("P.R. China"));
        assert!(!num_start().is_match("U.S. Government"));

        let ls = [
            line(
                "South suburb, Tai'an High-tech. Industrial Development Zone, Shandong Province of",
                0.20,
                0.92,
                0.30,
            ),
            line("P.R. China", 0.20, 0.30, 0.32),
        ];
        assert_eq!(texts(&ls).len(), 1, "这是地址的续行, 该并回去");
    }

    /// 悬挂缩进的续行要并回去 —— 缩进数字取自 Conclusion-for-QA 第 1 页实测
    ///
    /// 首行 0.079 在 "a." 上, 续行 0.107 在正文上, 差 0.028 恰好压过 x_tol
    #[test]
    fn hanging_indent_continuation_merges() {
        let ls = [
            line("a. VIP data is used for matching, but cannot provide data to strategy layer that live",
                 0.079, 0.926, 0.30),
            line("cannot obtain (Possible solution: Align VIP and live trading data)",
                 0.107, 0.405, 0.32),
        ];
        assert_eq!(texts(&ls).len(), 1, "悬挂缩进的续行, 该并回同一条列表项");
    }

    /// 但下一级子项自带序号, 照旧断开 —— 悬挂缩进不能把嵌套吃掉
    #[test]
    fn nested_marked_item_still_splits() {
        let ls = [
            line("a. Increase the aggregation of order flow to reduce the number of requests sent to the",
                 0.079, 0.926, 0.30),
            line("i. BookTicker", 0.109, 0.249, 0.32),
        ];
        assert_eq!(texts(&ls).len(), 2, "i. 是下一级子项, 不是悬挂缩进");
    }

    /// 只对列表项放行: 上一段不是列表项时, 右移仍然算换了缩进层级
    #[test]
    fn indent_shift_without_marker_still_splits() {
        let ls = [
            line("Unified DataFeed module, using UDP multicast and other forms for data distribution so",
                 0.046, 0.926, 0.30),
            line("that the strategy layer can subscribe on demand", 0.079, 0.405, 0.32),
        ];
        assert_eq!(texts(&ls).len(), 2, "上一段没有序号, 右移就是换了层级");
    }

    /// 左移一律断开: 那是退回外层, 不是悬挂缩进
    #[test]
    fn outdent_after_marker_still_splits() {
        let ls = [
            line("i. Idempotence class operations can be issued directly at the same time to multiple",
                 0.107, 0.926, 0.30),
            line("Latency of the whole link needs to be measured end to end", 0.046, 0.405, 0.32),
        ];
        assert_eq!(texts(&ls).len(), 2, "缩进退回外层, 不是续行");
    }
}
