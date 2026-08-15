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

        // 手写签名/印章: 边缘区 + 低置信度 + 极短
        if cfg.drop_stamp && (ry1 > 0.90 || ry0 < 0.05) && it.s < 0.88 && n <= 3 {
            continue;
        }
        // 左右页边被裁掉的竖排印记碎片("月""入""R公"...): 正文排不到那儿去。
        // 这里不看置信度 —— 实测这类碎片能到 0.99, 但它们会跨在两行之间, 把
        // 上下两行的包络搭起来并成一行, 危害比读出来的那一两个字大得多。
        if cfg.drop_stamp && (rx0 > 0.95 || rx1 < 0.05) && n <= 3 {
            continue;
        }
        // 骑缝章/手写签名: 短串 + 低置信; 印刷体正文均 > 0.95
        if cfg.drop_stamp && it.s < cfg.stamp_conf && n <= 4 && !short_label().is_match(&t) {
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
}
