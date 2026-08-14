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

fn footer_pat() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| {
        Regex::new(r"^(\d{1,3}\s*/\s*\d{1,3}$|\d{1,3}$|邮箱|地址|邮编|网址|http|tkdlsc)")
            .expect("页脚正则")
    })
}

fn short_label() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new(r"^[\dA-Za-z][\.、)）]?$").expect("短标号正则"))
}

/// 返回 (正文, 页眉, 页脚)
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
        } else if cfg.drop_footer
            && (ry0 > cfg.footer_y || (ry0 > 0.85 && footer_pat().is_match(&t)))
        {
            footer.push(rec);
        } else {
            body.push(rec);
        }
    }
    (body, header, footer)
}
