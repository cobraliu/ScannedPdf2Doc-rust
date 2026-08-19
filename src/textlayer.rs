//! 文本层: 把 PDF 自带的字符聚成跟 OCR 同构的行片段
//!
//! 出口是 `Vec<ocr::Item>`, 跟 [`crate::ocr::Engine::run`] 一字不差 —— 后面
//! 那一整套版面重建(视觉行、块划分、框线表、续行合并)于是原样复用, 这条路
//! 只换掉"字是怎么来的"。
//!
//! 框线仍然在页图上找: 文本层只有文字, 表格线是画出来的图形。所以走这条路
//! 省掉的是 OCR(九成时间), 不是渲染。

use crate::ocr::Item;
use crate::pdf::{Ch, VRule};

/// 行内断开的阈值: 空白比这个数乘字号还宽, 就切成两段
///
/// 切得粗是有讲究的。同一视觉行里的几段最后是拿 `join(" ")` 拼回去的
/// (见 layout::line::line_text 和 layout::para::build_rows), 切碎了中文正文
/// 会被塞进一堆空格。所以只在"列间距"这个量级上切:
///
/// - 密排汉字   间距 ≈ 0
/// - 英文词间   0.25 ~ 0.5 字宽; 两端对齐拉伸后最多到 1 倍上下
/// - 表格列之间 2 倍字宽起步
///
/// 1.2 落在后两者中间。切多了只是多出几个 item, 同一行的还会被视觉行聚类
/// 收回一起; 切少了才真丢东西 —— 表格的列会粘成一整行。
const BREAK: f32 = 1.2;

/// 同一行的判据: 字高的一半以内算同行
///
/// 不用"y 区间重叠比例": 一行里混着大小字号时(标题里嵌小字注释、上下标),
/// 小字的框整个落在大字框里面, 重叠比例是 1.0, 反过来只有 0.4, 谁并谁看
/// 遍历顺序。拿中心距比, 两边算出来一样。
fn same_row(a_cy: f32, a_h: f32, b_cy: f32, b_h: f32) -> bool {
    (a_cy - b_cy).abs() < 0.5 * a_h.min(b_h).max(1.0)
}

/// 中间隔着一条画出来的竖线
///
/// 那是单元格边界, 两个字挨得再紧也得断开。表头最典型: 相邻两格的字各自
/// 把格宽填满, 间距是 0, 只有这条线能把它们分开。
fn ruled_apart(vs: &[VRule], x0: f32, x1: f32, cy: f32) -> bool {
    vs.iter()
        .any(|v| x0 < v.x && v.x < x1 && v.y0 <= cy && cy <= v.y1)
}

/// 字符聚成行片段
///
/// 顺序按 (y, x) 排, 不按 PDF 内容流的顺序 —— 内容流里字的先后是排版软件
/// 写进去的, 分栏、脚注、表格都可能乱序, 拿它当阅读顺序会错得很难查。
///
/// `vrules` 是页面上画出来的竖线, 拿来当单元格边界; 没有框线的表和正文靠
/// 间距断, 见 [`BREAK`]。
pub fn items(chars: &[Ch], vrules: &[VRule]) -> Vec<Item> {
    if chars.is_empty() {
        return Vec::new();
    }
    let mut cs: Vec<&Ch> = chars.iter().collect();
    cs.sort_by(|a, b| {
        let (acy, bcy) = ((a.y0 + a.y1) / 2.0, (b.y0 + b.y1) / 2.0);
        acy.partial_cmp(&bcy)
            .unwrap()
            .then(a.x0.partial_cmp(&b.x0).unwrap())
    });

    // ---- 聚行 ----
    let mut rows: Vec<Vec<&Ch>> = Vec::new();
    for c in cs {
        let cy = (c.y0 + c.y1) / 2.0;
        let h = c.y1 - c.y0;
        let fit = rows.last().is_some_and(|r| {
            let (ry0, ry1) = r.iter().fold((f32::MAX, f32::MIN), |(lo, hi), k| {
                (lo.min(k.y0), hi.max(k.y1))
            });
            same_row((ry0 + ry1) / 2.0, ry1 - ry0, cy, h)
        });
        match (fit, rows.last_mut()) {
            (true, Some(r)) => r.push(c),
            _ => rows.push(vec![c]),
        }
    }

    // ---- 行内按空白切段 ----
    let mut out = Vec::new();
    for mut r in rows {
        r.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap());
        let cy = {
            let (lo, hi) = r.iter().fold((f32::MAX, f32::MIN), |(lo, hi), k| {
                (lo.min(k.y0), hi.max(k.y1))
            });
            (lo + hi) / 2.0
        };
        let mut seg: Vec<&Ch> = Vec::new();
        for c in r {
            let brk = seg.last().is_some_and(|p: &&Ch| {
                // 拿两个字的中心去问, 不拿它们的边: 字紧贴着框线时, 边会压过
                // 线去一两个像素, 用边判就漏了
                let (pc, cc) = ((p.x0 + p.x1) / 2.0, (c.x0 + c.x1) / 2.0);
                if ruled_apart(vrules, pc, cc, cy) {
                    return true;
                }
                // 尺子取两边字号里大的那个: 小字后面跟大字时, 拿小的量会把
                // 正常的一个空格判成断开
                c.x0 - p.x1 > BREAK * p.size.max(c.size).max(1.0)
            });
            if brk {
                push_seg(&mut out, &seg);
                seg.clear();
            }
            seg.push(c);
        }
        push_seg(&mut out, &seg);
    }
    out
}

fn push_seg(out: &mut Vec<Item>, seg: &[&Ch]) {
    let t: String = seg.iter().map(|c| c.c).collect();
    if t.trim().is_empty() {
        return;
    }
    let (x0, y0, x1, y1) = seg.iter().fold(
        (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
        |(a, b, c2, d), k| (a.min(k.x0), b.min(k.y0), c2.max(k.x1), d.max(k.y1)),
    );
    out.push(Item {
        t,
        x0,
        y0,
        x1,
        y1,
        // 文本层是照抄不是识别, 没有"可能认错"这回事。给满分还有一层作用:
        // noise.rs 拿低置信短串判印章噪声(stamp_conf 0.88), 给低了会被当噪声丢掉
        s: 1.0,
    });
}

/// 这一页的文本层够不够用
///
/// 三种页子都会走到这儿, 得分开:
///
/// - 原生 PDF: 字符成千上万, 直接用
/// - 纯扫描件: 一个字符没有, 只能 OCR
/// - 扫描件被别的工具加过 OCR 层: 字符有, 但质量取决于当初那个工具。这种
///   分不出好坏, 按"有就用"处理; 信不过的可以整份关掉(Config::text_layer)
///
/// 阈值卡在字符数而不是覆盖率: 封面页、只有一行标题的分隔页本来就没几个字,
/// 按覆盖率判会被踢去 OCR, 而 OCR 在这种页上并不会更准。
pub fn usable(chars: &[Ch]) -> bool {
    chars.iter().filter(|c| !c.c.is_whitespace()).count() >= MIN_CHARS
}

/// 少于这个数就当没有文本层
///
/// 扫描 PDF 常被塞进一点点文本层残渣: 页码、水印、制作软件写的标记。实测
/// 一位数到十几个字符不等, 20 能把它们挡在外面, 又拦不住真有内容的页。
const MIN_CHARS: usize = 20;

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一串同一行、字号 10、紧挨着排的字
    fn row(s: &str, x: f32, y: f32, size: f32) -> Vec<Ch> {
        s.chars()
            .enumerate()
            .map(|(i, c)| Ch {
                c,
                x0: x + i as f32 * size,
                y0: y,
                x1: x + (i + 1) as f32 * size,
                y1: y + size,
                size,
            })
            .collect()
    }

    #[test]
    fn keeps_a_line_of_chinese_in_one_piece() {
        let it = items(&row("合同编号", 100.0, 200.0, 10.0), &[]);
        assert_eq!(it.len(), 1, "密排汉字不能切开, 拼回去会多出空格");
        assert_eq!(it[0].t, "合同编号");
        assert_eq!((it[0].x0, it[0].x1), (100.0, 140.0));
    }

    #[test]
    fn breaks_where_a_table_column_starts() {
        let mut cs = row("名称", 100.0, 200.0, 10.0);
        cs.extend(row("数量", 200.0, 200.0, 10.0)); // 隔了 6 倍字宽
        let it = items(&cs, &[]);
        assert_eq!(it.len(), 2, "列之间要断开, 否则整行粘成一格");
        assert_eq!((it[0].t.as_str(), it[1].t.as_str()), ("名称", "数量"));
    }

    /// 两端对齐的英文段落会把词间距拉宽, 那不是列
    #[test]
    fn does_not_break_on_a_stretched_word_gap() {
        let mut cs = row("the", 100.0, 200.0, 10.0);
        cs.extend(row("quick", 140.0, 200.0, 10.0)); // 空了 1 倍字宽
        assert_eq!(items(&cs, &[]).len(), 1, "1 倍字宽还是词间距, 不是列间距");
    }

    #[test]
    fn splits_two_lines_and_reads_them_top_down() {
        let mut cs = row("第二行", 100.0, 230.0, 10.0);
        cs.extend(row("第一行", 100.0, 200.0, 10.0));
        let it = items(&cs, &[]);
        assert_eq!(it.len(), 2);
        assert_eq!(it[0].t, "第一行", "按坐标排, 不按内容流里的先后");
    }

    /// 上下标、行内小字注释的框整个套在大字框里, 得算同一行
    #[test]
    fn a_subscript_stays_on_its_line() {
        let mut cs = row("H", 100.0, 200.0, 10.0);
        cs.extend(row("2", 110.0, 204.0, 5.0));
        cs.extend(row("O", 115.0, 200.0, 10.0));
        let it = items(&cs, &[]);
        assert_eq!(it.len(), 1);
        assert_eq!(it[0].t, "H2O");
    }

    #[test]
    fn drops_pages_with_only_a_scrap_of_text_layer() {
        assert!(
            !usable(&row("第 3 页", 100.0, 200.0, 10.0)),
            "扫描件的页码残渣"
        );
        let long: Vec<Ch> = row(&"字".repeat(30), 100.0, 200.0, 10.0);
        assert!(usable(&long));
    }

    /// 表头里相邻两格的字各自把格宽填满, 中间一个像素的空隙都没有 ——
    /// 只有那条画出来的竖线能把它们分开。
    ///
    /// 数字取自 excel2pdf.pdf 第 1 页: 未修之前这两格被并成一句
    /// "其他程序化交易软程序化交易软件", 拼出来读不通。
    #[test]
    fn splits_two_touching_cells_at_the_rule() {
        let mut cs = row("其他程序化交易软", 1004.0, 963.0, 23.6);
        cs.extend(row("程序化交易软件", 1193.0, 963.0, 23.6));
        assert_eq!(items(&cs, &[]).len(), 1, "没有线的话确实分不开, 这是前提");

        let rule = VRule {
            x: 1193.0,
            y0: 900.0,
            y1: 1100.0,
        };
        let it = items(&cs, &[rule]);
        assert_eq!(it.len(), 2, "有线就得断开");
        assert_eq!(it[0].t, "其他程序化交易软");
        assert_eq!(it[1].t, "程序化交易软件");
    }

    /// 但线只管它自己那一段: 短竖线下面的行不该被它切开
    #[test]
    fn a_rule_only_splits_the_rows_it_spans() {
        let mut cs = row("名称", 100.0, 200.0, 10.0);
        cs.extend(row("数量", 120.0, 200.0, 10.0));
        let below = VRule {
            x: 120.0,
            y0: 400.0,
            y1: 500.0,
        };
        assert_eq!(items(&cs, &[below]).len(), 1, "线在下面几行, 管不着这行");
    }

    #[test]
    fn empty_page_yields_nothing() {
        assert!(items(&[], &[]).is_empty());
        assert!(!usable(&[]));
    }
}
