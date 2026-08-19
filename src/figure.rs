//! 印章、签名、插图 —— 认不出字的那些墨迹
//!
//! 识别管线只产出"哪儿有什么字", 一页上凡是不是字的东西到这一步就没了:
//! 合同末尾那个红章、手签的名字、规格书里的示意图和 logo, 转出来的 Word 里
//! 一概不见。丢的偏偏是最要紧的几样 —— 一份没有章的合同复印件是不作数的。
//!
//! 找法是"减法": 页面上的墨迹, 减去识别认下的字, 减去表格框线, 剩下的成团
//! 的东西就是它。这样一来印章、签名、插图、logo 用的是同一套判据, 不必各写
//! 一套(红章靠颜色找、签名靠笔画找……那些在灰度扫描件上一样也不成立)。
//!
//! 印章常常盖在字上。这没关系: 挖掉的是识别给出的字框, 章的圈和边上的字仍
//! 然留着, 圈够大就照样成团。
//!
//! 减法减得再干净, 还是会剩两类不该当图形的东西, 得靠已经认出来的版面挡掉:
//! 一是框线表里的零碎(格子边角、认漏的一两个字连着框线), 表本来就照原样还原
//! 成表了, 里面的东西不该再贴一张图; 二是页眉页脚那条横幅(抬头、logo、标语),
//! 正文里已经按页眉丢掉了, 再当插图贴回来就自相矛盾。

use crate::config::Config;
use crate::imgutil::{components_with_points, connected_components, Bitmap, Gray, Rgb};
use crate::layout::Page;
use crate::ocr::Item;

/// 墨迹阈值
const INK: u8 = 160;
/// 字框往外放这么多再挖 —— 识别给的框往往紧贴笔画, 不放一点会剩一圈毛边
const PAD: f32 = 3.0;
/// 比这更长的连续行程才可能是框线(按页宽的比例)
///
/// 不能设小。印章的圈、签名的一竖长度都在页宽的百分之几这个量级, 阈值一低
/// 就把它们当框线擦光了 —— 第一版设 0.05, 章和签名全军覆没。真框线是横贯
/// 半页的东西, 0.15 离它们还远。
const RULE: f32 = 0.15;
/// 还得够薄才算框线(按页宽的比例, 有下限)
///
/// 光看长度不够: 图表的外框、印章的直边都能画得很长。300 dpi 下扫描件的
/// 框线是两三个像素, 加粗的也就六七个; 十来像素往上就是别的东西了。
const RULE_THICK: f32 = 1.0 / 150.0;
/// 一个连通块小于页面这么多就是噪点, 不参与
const SPECK: f32 = 0.000_02;
/// 两团离得比这近就并成一处(按页宽的比例)
const NEAR: f32 = 0.02;
/// 一处图形至少要有这么宽/高(按页宽/页高的比例)
const MIN_W: f32 = 0.04;
const MIN_H: f32 = 0.02;
/// 一处图形的墨迹至少要占它自己外接框的这么多 —— 挡掉"几个碎点撑出一大片"
const DENSITY: f32 = 0.02;
/// 大到这个份上就不是插图, 是整页没认出来(阈值坏了/整页是照片), 不动它
const HOG: f32 = 0.7;
/// 裁出来时四周多留一点白边
const MARGIN: f32 = 0.004;

/// 页面上一处非文字的图形
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl Rect {
    fn w(&self) -> usize {
        self.x1 - self.x0
    }
    fn h(&self) -> usize {
        self.y1 - self.y0
    }
    fn near(&self, o: &Rect, gap: usize) -> bool {
        self.x0 < o.x1 + gap && o.x0 < self.x1 + gap && self.y0 < o.y1 + gap && o.y0 < self.y1 + gap
    }
    fn merge(&mut self, o: &Rect) {
        self.x0 = self.x0.min(o.x0);
        self.y0 = self.y0.min(o.y0);
        self.x1 = self.x1.max(o.x1);
        self.y1 = self.y1.max(o.y1);
    }
}

/// 挖掉识别认下的字, 再挖掉够长的横竖行程(框线)
fn leftover_ink(img: &Gray, items: &[Item]) -> Bitmap {
    let mut bw = img.binarize(INK);
    for it in items {
        let x0 = (it.x0 - PAD).max(0.0) as usize;
        let y0 = (it.y0 - PAD).max(0.0) as usize;
        let x1 = ((it.x1 + PAD) as usize).min(img.w);
        let y1 = ((it.y1 + PAD) as usize).min(img.h);
        for y in y0..y1 {
            for x in x0..x1 {
                bw.set(x, y, 0);
            }
        }
    }
    let k = ((img.w as f32 * RULE) as usize).max(40);
    let thin = ((img.w as f32 * RULE_THICK) as usize).max(8);
    for horiz in [true, false] {
        for (b, pts) in components_with_points(&bw.open_line(horiz, k)) {
            if b.w.min(b.h) > thin {
                continue; // 长是长, 可它厚 —— 不是框线
            }
            for (x, y) in pts {
                bw.set(x as usize, y as usize, 0);
            }
        }
    }
    bw
}

/// 把挨得近的团并成一处 —— 印章的圈跟圈里的字是分开的几块, 签名更是十几笔
fn clump(mut rs: Vec<Rect>, gap: usize) -> Vec<Rect> {
    loop {
        let mut out: Vec<Rect> = Vec::new();
        let mut merged = false;
        'next: for r in rs {
            for o in &mut out {
                if o.near(&r, gap) {
                    o.merge(&r);
                    merged = true;
                    continue 'next;
                }
            }
            out.push(r);
        }
        rs = out;
        if !merged {
            return rs;
        }
    }
}

/// 落在框线表里就不算图形 —— 重叠掉自己一半以上就算落在里面
fn inside_a_table(r: &Rect, page: &Page) -> bool {
    let area = (r.w() * r.h()).max(1) as f32;
    page.grids().iter().any(|g| {
        let x = (r.x1.min(g.x1 as usize)).saturating_sub(r.x0.max(g.x0 as usize));
        let y = (r.y1.min(g.y1 as usize)).saturating_sub(r.y0.max(g.y0 as usize));
        (x * y) as f32 > area * 0.5
    })
}

/// 这一页上认不出字的成团墨迹
pub fn find(img: &Gray, items: &[Item], page: &Page, cfg: &Config) -> Vec<Rect> {
    if !cfg.keep_figures || img.w == 0 || img.h == 0 {
        return Vec::new();
    }
    let (w, h) = (img.w as f32, img.h as f32);
    let bw = leftover_ink(img, items);
    let speck = (w * h * SPECK) as usize;
    let blobs: Vec<Rect> = connected_components(&bw)
        .into_iter()
        .filter(|b| b.area >= speck)
        .map(|b| Rect {
            x0: b.x,
            y0: b.y,
            x1: b.x + b.w,
            y1: b.y + b.h,
        })
        .collect();

    // 密度按团的实际墨量算, 不按并完的大框 —— 并的过程只长框不长墨
    let ink: Vec<usize> = blobs
        .iter()
        .map(|r| {
            let mut n = 0;
            for y in r.y0..r.y1 {
                for x in r.x0..r.x1 {
                    n += usize::from(bw.at(x, y) != 0);
                }
            }
            n
        })
        .collect();

    let gap = (w * NEAR) as usize;
    let mut out = Vec::new();
    for r in clump(blobs.clone(), gap) {
        if (r.w() as f32) < w * MIN_W || (r.h() as f32) < h * MIN_H {
            continue;
        }
        if r.w() as f32 > w * HOG && r.h() as f32 > h * HOG {
            continue;
        }
        let mine: usize = blobs
            .iter()
            .zip(&ink)
            .filter(|(b, _)| b.x0 >= r.x0 && b.x1 <= r.x1 && b.y0 >= r.y0 && b.y1 <= r.y1)
            .map(|(_, n)| *n)
            .sum();
        if (mine as f32) < r.w() as f32 * r.h() as f32 * DENSITY {
            continue;
        }
        // 页眉页脚那条带子上的东西正文里已经丢过一次了
        let (ry0, ry1) = (r.y0 as f32 / h, r.y1 as f32 / h);
        if ry1 < cfg.header_y || ry0 > cfg.footer_y {
            continue;
        }
        if inside_a_table(&r, page) {
            continue;
        }
        let m = (w * MARGIN) as usize;
        out.push(Rect {
            x0: r.x0.saturating_sub(m),
            y0: r.y0.saturating_sub(m),
            x1: (r.x1 + m).min(img.w),
            y1: (r.y1 + m).min(img.h),
        });
    }
    out.sort_by_key(|r| (r.y0, r.x0));
    out
}

/// 从彩色页图上裁一块, 编码成 PNG
pub fn crop_png(rgb: &Rgb, r: &Rect) -> Option<Vec<u8>> {
    let (w, h) = (
        r.x1.min(rgb.w).checked_sub(r.x0)?,
        r.y1.min(rgb.h).checked_sub(r.y0)?,
    );
    if w == 0 || h == 0 {
        return None;
    }
    let mut px = Vec::with_capacity(w * h * 3);
    for y in r.y0..r.y0 + h {
        let row = (y * rgb.w + r.x0) * 3;
        px.extend_from_slice(&rgb.px[row..row + w * 3]);
    }
    let img = image::RgbImage::from_raw(w as u32, h as u32, px)?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(w: usize, h: usize) -> Gray {
        Gray {
            w,
            h,
            px: vec![255; w * h],
        }
    }

    fn ink(g: &mut Gray, x0: usize, y0: usize, x1: usize, y1: usize) {
        let w = g.w;
        for y in y0..y1 {
            for x in x0..x1 {
                g.px[y * w + x] = 20;
            }
        }
    }

    /// 印章的圈。画成真圆的 —— 方环的四条边就是四条又长又直的线,
    /// 会被当框线剔掉, 那是测具自己造出来的假象, 不是章的样子
    fn ring(g: &mut Gray, cx: usize, cy: usize, r: usize, t: usize) {
        let w = g.w;
        for y in cy - r..=cy + r {
            for x in cx - r..=cx + r {
                let d = (((x as f32 - cx as f32).powi(2) + (y as f32 - cy as f32).powi(2)).sqrt()
                    - r as f32)
                    .abs();
                if d <= t as f32 / 2.0 {
                    g.px[y * w + x] = 20;
                }
            }
        }
    }

    fn item(x0: f32, y0: f32, x1: f32, y1: f32) -> Item {
        Item {
            t: "字".into(),
            x0,
            y0,
            x1,
            y1,
            s: 0.99,
        }
    }

    fn cfg() -> Config {
        Config::default()
    }

    /// 没表没页眉的空版面 —— 大部分判据不需要它
    fn bare() -> Page {
        Page {
            blocks: vec![],
            header: vec![],
            footer: vec![],
            w: 1200.0,
            h: 1600.0,
        }
    }

    #[test]
    fn finds_a_stamp_sitting_next_to_the_text() {
        let mut g = paper(1200, 1600);
        for r in 0..20 {
            ink(&mut g, 100, 100 + r * 40, 700, 100 + r * 40 + 24);
        }
        let items: Vec<Item> = (0..20)
            .map(|r| {
                item(
                    100.0,
                    (100 + r * 40) as f32,
                    700.0,
                    (100 + r * 40 + 24) as f32,
                )
            })
            .collect();
        ring(&mut g, 900, 1300, 110, 14);
        ink(&mut g, 850, 1280, 950, 1320);

        let f = find(&g, &items, &bare(), &cfg());
        assert_eq!(f.len(), 1, "该找到一处: {f:?}");
        let r = f[0];
        assert!(r.x0 < 795 && r.x1 > 1005, "该把整个章圈进去: {r:?}");
        assert!(r.y0 < 1195 && r.y1 > 1405, "该把整个章圈进去: {r:?}");
    }

    #[test]
    fn a_page_of_plain_text_yields_nothing() {
        let mut g = paper(1200, 1600);
        let mut items = Vec::new();
        for r in 0..30 {
            ink(&mut g, 100, 100 + r * 45, 1000, 100 + r * 45 + 28);
            items.push(item(
                100.0,
                (100 + r * 45) as f32,
                1000.0,
                (100 + r * 45 + 28) as f32,
            ));
        }
        assert!(
            find(&g, &items, &bare(), &cfg()).is_empty(),
            "全是字, 不该有图形"
        );
    }

    #[test]
    fn table_rules_are_not_figures() {
        let mut g = paper(1200, 1600);
        for y in [200, 500, 800] {
            ink(&mut g, 100, y, 1100, y + 4);
        }
        for x in [100, 600, 1100] {
            ink(&mut g, x, 200, x + 4, 804);
        }
        assert!(find(&g, &[], &bare(), &cfg()).is_empty(), "框线不是插图");
    }

    #[test]
    fn a_signature_scrawl_is_kept_as_one_piece() {
        // 手签是断开的十几笔, 得并成一处而不是十几处
        let mut g = paper(1200, 1600);
        for i in 0..12 {
            ink(
                &mut g,
                300 + i * 22,
                1200 + (i % 3) * 12,
                300 + i * 22 + 9,
                1260,
            );
        }
        let f = find(&g, &[], &bare(), &cfg());
        assert_eq!(f.len(), 1, "十几笔该并成一处: {f:?}");
    }

    #[test]
    fn scattered_specks_are_not_a_figure() {
        let mut g = paper(1200, 1600);
        for i in 0..8 {
            ink(&mut g, 100 + i * 120, 800, 100 + i * 120 + 6, 806);
        }
        assert!(
            find(&g, &[], &bare(), &cfg()).is_empty(),
            "几个碎点不算图形"
        );
    }

    #[test]
    fn a_whole_page_of_ink_is_left_alone() {
        // 整页发黑多半是阈值出了问题, 不是有一张满页插图
        let mut g = paper(1200, 1600);
        ink(&mut g, 20, 20, 1180, 1580);
        assert!(find(&g, &[], &bare(), &cfg()).is_empty(), "整页黑不当插图");
    }

    #[test]
    fn the_switch_turns_it_off() {
        let mut g = paper(1200, 1600);
        ring(&mut g, 900, 1300, 110, 14);
        let mut c = cfg();
        c.keep_figures = false;
        assert!(find(&g, &[], &bare(), &c).is_empty());
    }

    #[test]
    fn crops_out_a_colour_patch() {
        let rgb = Rgb {
            w: 40,
            h: 40,
            px: (0..40 * 40).flat_map(|i| [(i % 255) as u8, 7, 9]).collect(),
        };
        let png = crop_png(
            &rgb,
            &Rect {
                x0: 5,
                y0: 5,
                x1: 25,
                y1: 30,
            },
        )
        .expect("该裁得出来");
        assert_eq!(&png[1..4], b"PNG");
        let back = image::load_from_memory(&png)
            .expect("PNG 该读得回来")
            .into_rgb8();
        assert_eq!((back.width(), back.height()), (20, 25));
        // 裁出来的左上角对应原图 (5,5), 也就是第 5*40+5 = 205 个像素
        assert_eq!(back.get_pixel(0, 0).0, [205, 7, 9]);
    }
}
