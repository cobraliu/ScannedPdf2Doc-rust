//! 光照摊平 —— 手机拍的纸一边亮一边暗, 先把纸面拉回一样白
//!
//! 扫描仪出来的页面本来就是平的, 这一步对它是空转。真正要治的是相机:
//! 台灯在左手边, 页面右下角比左上角暗五六十级是常事。后面每一步都吃这个亏 ——
//! 框线检测是一个全局阈值(`grid.rs` 的 `RULE_DARK`), 暗的那半边整片算墨迹、
//! 亮的那半边细线又够不着阈值; 去歪斜数墨点用的也是固定阈值, 同样偏。
//!
//! 做法是最老实的一种: 估出「这块地方的纸有多白」, 再把每个像素按它归一化。
//! 估纸面亮度按格子取高分位数 —— 一格里绝大多数像素本来就是纸, 取第 90
//! 百分位就避开了字。整格被印章或照片盖死时这个数会塌下去, 得让旁边的纸把它
//! 填回来, 否则黑块会被除成白板。填的规矩是「只补明显塌下去的格」: 比邻格的
//! 亮处低 SLACK 级以上才认作没纸可看, 照邻格补; 差得不多就是正常渐变, 留着 ——
//! 无脑取大会把暗侧的纸整体估高一格的渐变量, 摊完那边还是灰的。
//!
//! 摊完不会正好 255。一格里取高分位数, 取到的是这格最亮那侧的纸, 当成格心
//! 的值用就偏了半格 —— 凡是"取局部亮处"的背景估计在渐变上都有这个偏置。偏
//! 多少等于一格宽度上的渐变量: 手机那种一页 80 级的阴影摊到 96 px 的格上是
//! 两三级, 无所谓; 造一个陡到一格 18 级的极端梯度, 残留也就六级。要紧的是
//! 不匀从几十上百级掉到个位数, 纸稳稳站在阈值那一边, 而不是数字正好是 255。

use crate::imgutil::Gray;

/// 背景网格的格子边长(像素)
///
/// 不能小。阴影是低频的东西, 格子只要够密到跟得上它就行; 格子一小, 一格就
/// 装不下几行字, 整格被文字填满时分位数会塌下去 —— 实测 48 px 的格子在一份
/// 本来就很平的扫描件上误报「差 25 级」, 摊完丢了一个字。96 px 是三四行字高,
/// 纸稳稳占多数。
const TILE: usize = 96;
/// 一格里第几百分位算「纸」
const PAPER: usize = 95;
/// 明暗差不到这么多级就当它本来就平, 原样放过
///
/// 门槛压在估计器自己的噪声之上。页面再平, 排得密的那些格里纸也占不到压倒
/// 多数, 分位数会晃 —— 实测一份平整扫描件最密的一页晃到 15 级。门槛设 12 时
/// 这页被当成不匀摊了一道, 代价是签章栏里一个淡淡的"电话："没认出来。
///
/// 另一头也够宽松: 阴影要压到纸面接近"算墨迹"的阈值才真的有害, 那是六十级
/// 往上的事。32 离噪声底两倍远, 离真正要治的还差得远, 两边都不沾。
const FLAT: u8 = 32;
/// 背景估计的下限: 再暗的地方也不当纸看, 否则除出来全是噪点放大
const FLOOR: u8 = 48;
/// 比邻格暗这么多才算「这格没纸可看」, 照邻格补
const SLACK: u8 = 30;
/// 补几遍。一遍往里推一格, 够盖住 8 格宽(约 800 px)的整块黑
const PASSES: usize = 8;

/// 格子网上的纸面亮度估计
struct Bg {
    w: usize,
    h: usize,
    v: Vec<u8>,
}

impl Bg {
    #[inline]
    fn at(&self, x: usize, y: usize) -> f32 {
        self.v[y.min(self.h - 1) * self.w + x.min(self.w - 1)] as f32
    }
}

/// 按格子取高分位, 再 3x3 取大填掉被黑块压塌的格
fn background(g: &Gray) -> Bg {
    let (gw, gh) = (g.w.div_ceil(TILE), g.h.div_ceil(TILE));
    let mut raw = vec![0u8; gw * gh];
    let mut buf = Vec::with_capacity((TILE / 2 + 1) * (TILE / 2 + 1));
    for ty in 0..gh {
        for tx in 0..gw {
            buf.clear();
            // 隔一个取一个: 一格几百个样本够定分位数了, 省一半时间
            for y in (ty * TILE..((ty + 1) * TILE).min(g.h)).step_by(2) {
                let row = &g.px[y * g.w..(y + 1) * g.w];
                for x in (tx * TILE..((tx + 1) * TILE).min(g.w)).step_by(2) {
                    buf.push(row[x]);
                }
            }
            if buf.is_empty() {
                continue;
            }
            let k = (buf.len() - 1) * PAPER / 100;
            let (_, m, _) = buf.select_nth_unstable(k);
            raw[ty * gw + tx] = *m;
        }
    }
    let mut v = raw;
    for _ in 0..PASSES {
        let src = v.clone();
        let mut moved = false;
        for ty in 0..gh {
            for tx in 0..gw {
                let me = src[ty * gw + tx];
                let mut hi = me;
                for ny in ty.saturating_sub(1)..(ty + 2).min(gh) {
                    for nx in tx.saturating_sub(1)..(tx + 2).min(gw) {
                        hi = hi.max(src[ny * gw + nx]);
                    }
                }
                if hi.saturating_sub(me) > SLACK {
                    v[ty * gw + tx] = hi;
                    moved = true;
                }
            }
        }
        if !moved {
            break;
        }
    }
    for b in &mut v {
        *b = (*b).max(FLOOR);
    }
    Bg { w: gw, h: gh, v }
}

/// 这一页最亮处和最暗处的纸面差了多少级
fn spread(bg: &Bg) -> u8 {
    let hi = bg.v.iter().copied().max().unwrap_or(0);
    let lo = bg.v.iter().copied().min().unwrap_or(0);
    hi - lo
}

/// 按背景把每个像素归一化到「纸 = 255」
fn divide(g: &Gray, bg: &Bg) -> Gray {
    let mut px = vec![0u8; g.px.len()];
    for y in 0..g.h {
        // 格心在 TILE/2 处, 所以减半格再插值, 不然整幅图会偏移半格
        let fy = (y as f32 + 0.5) / TILE as f32 - 0.5;
        let (y0, ry) = (fy.floor().max(0.0) as usize, fy - fy.floor().max(0.0));
        for x in 0..g.w {
            let fx = (x as f32 + 0.5) / TILE as f32 - 0.5;
            let (x0, rx) = (fx.floor().max(0.0) as usize, fx - fx.floor().max(0.0));
            let top = bg.at(x0, y0) + rx * (bg.at(x0 + 1, y0) - bg.at(x0, y0));
            let bot = bg.at(x0, y0 + 1) + rx * (bg.at(x0 + 1, y0 + 1) - bg.at(x0, y0 + 1));
            let b = top + ry * (bot - top);
            let v = g.px[y * g.w + x] as f32 * 255.0 / b.max(1.0);
            px[y * g.w + x] = v.clamp(0.0, 255.0) as u8;
        }
    }
    Gray { w: g.w, h: g.h, px }
}

/// 量一下这页光照匀不匀; 不匀就摊平, 返回摊掉的级差(平的页面返回 0)
pub fn flatten(g: &mut Gray) -> u8 {
    if g.w < TILE || g.h < TILE {
        return 0;
    }
    let bg = background(g);
    let d = spread(&bg);
    if d < FLAT {
        return 0;
    }
    *g = divide(g, &bg);
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(w: usize, h: usize, v: u8) -> Gray {
        Gray {
            w,
            h,
            px: vec![v; w * h],
        }
    }

    fn ink(g: &mut Gray, x0: usize, y0: usize, x1: usize, y1: usize, v: u8) {
        let w = g.w;
        for y in y0..y1 {
            for x in x0..x1 {
                g.px[y * w + x] = v;
            }
        }
    }

    /// 左边亮右边暗: 纸从 250 一路掉到 140 —— 右边一截已经比 RULE_DARK 还暗
    fn shaded() -> Gray {
        let mut g = page(600, 400, 0);
        for y in 0..400 {
            for x in 0..600 {
                g.px[y * 600 + x] = (250.0 - 110.0 * x as f32 / 600.0) as u8;
            }
        }
        g
    }

    #[test]
    fn leaves_an_evenly_lit_page_alone() {
        let mut g = page(600, 400, 250);
        ink(&mut g, 100, 100, 300, 130, 40);
        let before = g.px.clone();
        assert_eq!(flatten(&mut g), 0, "本来就平, 不该动它");
        assert_eq!(g.px, before);
    }

    #[test]
    fn a_page_packed_with_text_is_not_mistaken_for_uneven() {
        // 光是均匀的, 只是字排得满 —— 格子里纸占不到压倒多数, 分位数会晃。
        // 把这个晃动当成阴影去摊, 淡字会被摊没(实测丢过一个"电话：")
        let mut g = page(600, 400, 250);
        for r in 0..12 {
            for c in 0..14 {
                ink(
                    &mut g,
                    20 + c * 40,
                    20 + r * 32,
                    20 + c * 40 + 30,
                    20 + r * 32 + 22,
                    30,
                );
            }
        }
        let before = g.px.clone();
        assert_eq!(flatten(&mut g), 0, "满页字不是光照不匀");
        assert_eq!(g.px, before);
    }

    #[test]
    fn pulls_the_dark_side_back_up_to_white() {
        let mut g = shaded();
        let (bl, br) = (g.at(20, 200), g.at(580, 200));
        assert!(bl - br > 100, "先确认这页确实很不匀: 左 {bl} 右 {br}");

        assert!(flatten(&mut g) >= FLAT, "该认出这页不匀");
        let (l, r) = (g.at(20, 200), g.at(580, 200));
        assert!(l > 235 && r > 235, "两边的纸都该回到白: 左 {l} 右 {r}");
        // 这才是重点: 一百多级的落差塌成个位数。残留的那几级是半格偏置,
        // 见文件开头 —— 这个梯度陡到一格 18 级, 比手机拍的极端得多
        assert!(l.abs_diff(r) < 10, "落差该没了: 左 {l} 右 {r}");
    }

    #[test]
    fn ink_stays_dark_on_both_sides() {
        let mut g = shaded();
        // 两侧各写一笔, 都是当地纸面的两成 —— 暗那侧的字比亮那侧的纸还暗得多
        ink(&mut g, 60, 180, 140, 220, 50);
        ink(&mut g, 460, 180, 540, 220, 30);
        flatten(&mut g);
        assert!(g.at(100, 200) < 80, "左边的字: {}", g.at(100, 200));
        assert!(g.at(500, 200) < 80, "右边的字: {}", g.at(500, 200));
    }

    #[test]
    fn a_stamp_does_not_get_bleached() {
        let mut g = shaded();
        // 整整两格宽的黑块: 格内分位数会塌到 0, 靠邻格取大才救得回来
        ink(&mut g, 200, 150, 320, 270, 10);
        flatten(&mut g);
        assert!(g.at(260, 210) < 60, "黑块该还是黑的: {}", g.at(260, 210));
    }

    #[test]
    fn a_global_threshold_stops_calling_the_dark_paper_ink() {
        // 框线检测就是一个全局阈值(grid.rs 的 RULE_DARK = 185)。暗那侧的白纸
        // 自己就掉到 185 以下, 于是整片空白被当成墨迹 —— 找线的先被喂了一版
        // 大黑块。这是阴影最先弄坏的东西, 不是"线看不见了"
        let mut g = shaded();
        let false_ink = |g: &Gray| (0..600).filter(|&x| g.at(x, 200) < 185).count() * 100 / 600;
        assert!(
            false_ink(&g) > 25,
            "摊平前暗侧的纸该被误判成墨: {}%",
            false_ink(&g)
        );
        flatten(&mut g);
        assert_eq!(false_ink(&g), 0, "摊平后一处也不该误判");
    }

    #[test]
    fn survives_a_blank_or_tiny_page() {
        let mut blank = page(600, 400, 255);
        assert_eq!(flatten(&mut blank), 0);
        let mut tiny = page(10, 10, 200);
        assert_eq!(flatten(&mut tiny), 0);
        let mut empty = Gray {
            w: 0,
            h: 0,
            px: vec![],
        };
        assert_eq!(flatten(&mut empty), 0);
    }
}
