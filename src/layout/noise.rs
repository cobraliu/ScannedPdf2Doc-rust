//! 噪声过滤: 剔除签名/印章/装订线碎片, 并把页眉页脚分离出来

use regex::Regex;
use std::sync::OnceLock;

use super::{clean, Box2};
use crate::config::Config;
use crate::ocr::Item;

fn header_pat() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| {
        // 别用行末反斜杠折行: 裸字符串里反斜杠不是续行符, 会给下一支正则
        // 前面粘一个转义换行, 那一支就永远匹配不上了。要折行用 concat!。
        Regex::new(concat!(
            r"^(TROESTER|EXCELLENCE IN EXTRUSION|Contract No\.|January \d+|Attachment \d|",
            r"De?a?tailed Technical Description|Page\s*\d+\s*/\s*\d+|正直诚信|山东泰开电缆有限公司$)",
        ))
        .expect("页眉正则")
    })
}

/// 页脚长什么样 —— 页码, 或者抬头里那几项联系方式
///
/// `-1-` `-2.` 这类加了破折号的页码原来漏在外面: 配套.pdf 十一页的页码全是
/// 这个式样, 靠"落在页脚区就丢"才没露出来, 判据改成认内容之后就得写进来。
/// 识别把右边那道破折号认成句点是常事, 所以右边界放宽到 - — – . 。都算。
fn footer_pat() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| {
        Regex::new(concat!(
            r"^(\d{1,3}\s*/\s*\d{1,3}$|\d{1,3}$|",
            r"[-—–]\s*\d{1,3}\s*[-—–.．。]?$|",
            r"邮箱|地址|邮编|网址|http|tkdlsc)",
        ))
        .expect("页脚正则")
    })
}

fn short_label() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new(r"^[\dA-Za-z][\.、)）]?$").expect("短标号正则"))
}

/// 这一小块是成句的形状, 不是印记碎片
///
/// "短 + 置信度低"是印章和手写签名的样子, 可正文里也有这么短的东西: 段落最后
/// 一行常常只剩一两个字(配套.pdf 第 11 页 "……单独确定保修" 的下一行就只有
/// "期。"), 清单的标签列也短("• 材料" "• 料斗")。手机拍的页面整体置信度比平板
/// 扫描低一截, 这些短行成片地掉到 stamp_conf 以下, 于是整行凭空消失 —— 段落
/// 断在半截, 段首那句不再以句号收尾, heading_level 跟着把它判成小标题加粗,
/// 一处丢字带出两处错。
///
/// 印记碎片认不出成句的形状: 它们是转圈排的印章文字被拆散的片段("泊上城"
/// "马细片"), 或者干脆是笔画噪声("u4" "\"x.V")。所以按"有没有文档结构的形状"
/// 放行: 句末标点收尾的是一句话的尾巴, 项目符号或编号开头的是清单里的一条。
///
/// 光一个字母或数字("u" "F" "3.")不算 —— 那既可能是标号也可能是印记的一角,
/// 形状里没有信息, 归 `short_label` 单独管。
fn looks_like_prose(t: &str) -> bool {
    static TAIL: OnceLock<Regex> = OnceLock::new();
    static HEAD: OnceLock<Regex> = OnceLock::new();
    // 只认全角的句末标点。半角 "." 和 ":" 得排除: "-2." 是页码, 单个 ":" 是
    // 表格框线被认成了字, 这两样本来就该丢
    let tail = TAIL.get_or_init(|| Regex::new(r"[。！？；：]$").expect("句尾正则"));
    // 圆点开头允许跟个空格也允许不跟("• 材料" 跟 "•料斗" 同一页上都有);
    // 后面必须还有字, 光一个圆点是噪声。识别常把 • 认成 。, 所以开头的 。 也
    // 算一种圆点 —— 句号开头本来就不是正常写法
    let head = HEAD.get_or_init(|| {
        Regex::new(
            r"^([•·●○。]\s*\S|\d{1,2}[\.、]\s*\S|[（(]\d{1,2}[）)]|[一二三四五六七八九十]+[、．.])",
        )
        .expect("清单开头正则")
    });
    tail.is_match(t) || head.is_match(t)
}

/// 返回 (正文, 页眉, 页脚)
///
/// 页眉页脚都是"位置对 + 内容像"两个条件都要。原先页脚还有一支光看位置:
/// 落在 footer_y 以下就整行丢掉, 不问写的是什么。可正文本来就能排到页面
/// 底部 —— Conclusion-for-QA 第 1 页末尾那条 "1. Inter-strategy priority:
/// high frequency priority" 落在 0.921, 于是凭空消失, 下一页从 "2." 开始,
/// 整段大纲看着就是错的。3#线 96 页里这么丢掉的有 135 行, 最长 109 个字。
///
/// 位置只是必要条件, 判不判是页脚得由内容说了算。
pub fn drop_noise(
    items: &[Item],
    w: f32,
    h: f32,
    cfg: &Config,
) -> (Vec<Box2>, Vec<Box2>, Vec<Box2>) {
    let (mut body, mut header, mut footer) = (Vec::new(), Vec::new(), Vec::new());
    for it in items {
        let t = clean(&it.t);
        if t.is_empty() {
            continue;
        }
        let (ry0, ry1) = (it.y0 / h, it.y1 / h);
        let (rx0, rx1) = (it.x0 / w, it.x1 / w);
        let n = t.chars().count();

        // 形状像正文的短行留下来 —— 但只在正文排得到的地方算数。左右页边那
        // 一条竖着的印记碎片里也有"1.公司"这种像编号的, 放它进来会把上下两行
        // 的包络搭成一行, 表当场塌掉(配套.pdf 第 7 页那张参数表就是这么没的)。
        let inside = rx0 <= 0.95 && rx1 >= 0.05;
        let prose = inside && looks_like_prose(&t);

        // 手写签名/印章: 边缘区 + 低置信度 + 极短
        if cfg.drop_stamp && (ry1 > 0.90 || ry0 < 0.05) && it.s < 0.88 && n <= 3 && !prose {
            continue;
        }
        // 左右页边被裁掉的竖排印记碎片("月""入""R公"...): 正文排不到那儿去。
        // 这里不看置信度 —— 实测这类碎片能到 0.99, 但它们会跨在两行之间, 把
        // 上下两行的包络搭起来并成一行, 危害比读出来的那一两个字大得多。
        //
        // 这条也不看"像不像正文": 位置已经说明了一切, 正文的行排不到 rx0>0.95
        // 那儿去; 真在那儿的"1."就是骑缝章上的字, 放行反而把并行的祸根留下。
        if cfg.drop_stamp && (rx0 > 0.95 || rx1 < 0.05) && n <= 3 {
            continue;
        }
        // 骑缝章/手写签名: 短串 + 低置信; 印刷体正文均 > 0.95
        if cfg.drop_stamp
            && it.s < cfg.stamp_conf
            && n <= 4
            && !prose
            && !short_label().is_match(&t)
        {
            continue;
        }

        let rec = Box2::from_item(it, w, h, t.clone());
        if cfg.drop_header && ry1 < cfg.header_y && header_pat().is_match(&t) {
            header.push(rec);
        } else if cfg.drop_footer && ry0 > cfg.footer_y && footer_pat().is_match(&t) {
            footer.push(rec);
        } else {
            body.push(rec);
        }
    }
    (body, header, footer)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 页高 1000, 给一行落在 y 处、宽度占半页的文字
    fn item(t: &str, y: f32) -> Item {
        Item {
            t: t.into(),
            x0: 100.0,
            y0: y,
            x1: 500.0,
            y1: y + 25.0,
            s: 0.99,
        }
    }

    fn split(items: &[Item]) -> (Vec<String>, Vec<String>) {
        let cfg = Config::default();
        let (body, _, foot) = drop_noise(items, 1000.0, 1000.0, &cfg);
        (
            body.into_iter().map(|b| b.t).collect(),
            foot.into_iter().map(|b| b.t).collect(),
        )
    }

    /// 排到页面底部的正文不能当页脚丢掉
    ///
    /// Conclusion-for-QA 第 1 页末尾这条落在 0.921, 原先光按位置就删了,
    /// 下一页于是从 "2." 开始
    #[test]
    fn body_text_at_the_bottom_survives() {
        let (body, foot) = split(&[item(
            "1. Inter-strategy priority: high frequency priority",
            921.0,
        )]);
        assert_eq!(body.len(), 1, "正文该留下");
        assert!(foot.is_empty());
    }

    /// 页码照样丢
    #[test]
    fn page_numbers_are_still_dropped() {
        for t in ["7", "3 / 96", "-1-", "-2.", "邮编：271000"] {
            let (body, foot) = split(&[item(t, 921.0)]);
            assert!(body.is_empty() && foot.len() == 1, "{t:?} 该当页脚丢掉");
        }
    }

    /// 长得像页脚但没落在页脚区的, 是正文
    #[test]
    fn a_footer_looking_line_up_the_page_is_body() {
        let (body, foot) = split(&[item("7", 400.0)]);
        assert_eq!(body.len(), 1);
        assert!(foot.is_empty());
    }

    /// 段落最后那行只剩一两个字、置信度又低 —— 不能当印记删掉
    ///
    /// 手机拍的页面整体置信度比扫描低一截, 配套.pdf 那句 "……单独确定保修" 的
    /// 下一行只有 "期。", 到手机上就掉到 stamp_conf 以下。删了它段落断在半截,
    /// 段首那句不再以句号收尾, 还会被 heading_level 判成小标题加粗。
    /// 页面中间和页面底部两处都要留住 —— 底部那处走的是另一条判据。
    #[test]
    fn a_short_low_confidence_sentence_tail_survives() {
        for y in [450.0, 920.0] {
            let (body, foot) = split(&[Item {
                s: 0.80,
                ..item("期。", y)
            }]);
            assert_eq!(body.len(), 1, "y={y} 处句号收尾的尾行是正文");
            assert!(foot.is_empty());
        }
    }

    /// 清单的标签列一样短
    ///
    /// 3#线 那张"标签—值"表里 "• 材料" 掉了, 右边那格"渗氮钢"还留着, 于是三行
    /// 的值挤进同一格, 整张表塌掉。识别常把 • 认成 。, 所以 "。 设计" 也算一条。
    #[test]
    fn a_short_low_confidence_list_label_survives() {
        for t in ["• 材料", "•料斗", "。 设计", "1.公司"] {
            let (body, _) = split(&[Item {
                s: 0.75,
                ..item(t, 500.0)
            }]);
            assert_eq!(body.len(), 1, "{t:?} 是清单里的一条");
        }
    }

    /// 印记碎片照旧丢: 转圈排的印章文字拆出来的片段认不出成句的形状
    #[test]
    fn stamp_fragments_are_still_dropped() {
        for t in ["泊上城", "马细片", "u4", "上场", "真"] {
            let (body, _) = split(&[Item {
                s: 0.75,
                ..item(t, 500.0)
            }]);
            assert!(body.is_empty(), "{t:?} 是印记碎片, 该丢");
        }
    }

    /// 页边那条竖着的印记里也有像编号的, 那儿一律不放行
    ///
    /// 配套.pdf 第 7 页右页边有个 "1.公司"(x 从 0.971 起), 形状像清单的一条,
    /// 放进来它会把上下两行的包络搭成一行 —— 那张参数表当场塌成一行文字。
    #[test]
    fn a_list_looking_fragment_at_the_page_edge_is_still_dropped() {
        let (body, _) = split(&[Item {
            s: 0.63,
            x0: 971.0,
            x1: 1000.0,
            ..item("1.公司", 500.0)
        }]);
        assert!(body.is_empty(), "页边的编号是骑缝章上的字");
    }
}
