//! 摆正: 量出扫描歪了多少度, 转回去
//!
//! 实测手上的样张: 配套.pdf 每页歪 0.7~1.1 度, 3#线.pdf 0.2~0.4 度。一度
//! 听着不多, 但在 1800 px 宽的页面上, 一条横线两端差了 31 个像素 —— 框线
//! 检测那套形态学是拿水平结构元开运算, 一条歪着的线会被打断成好几截,
//! grid.rs 里那个 stitch_v 就是在给这件事擦屁股。摆正是治本的一步。
//!
//! 只在识别那条路上做。走文字层的页不能转: 那些字的坐标来自 PDF 文件本身,
//! 图一转就跟坐标对不上了 —— 何况原生 PDF 本来也不歪。

use crate::imgutil::Gray;

/// 算墨迹的阈值, 跟正文判深浅用的是同一个数
const INK: u8 = 160;

/// 认得出的最大歪斜
///
/// 超过这个数的多半不是扫歪了, 是整页横放或者倒着放 —— 那是另一回事,
/// 硬按歪斜转只会把好页转坏。
const MAX_DEG: f32 = 3.0;

/// 小于这个角度就不折腾
///
/// 转一次要重采样一次, 字会稍微糊一点点。0.1 度在整页宽度上只错开 3 个
/// 像素, 治不了什么病, 却要全页付一次插值的代价。
const MIN_DEG: f32 = 0.15;

/// 把墨迹按角度 `t`(正切值)投影到各行, 返回相邻行墨量差的平方和
///
/// 页面摆正时每一行要么是字要么是空白, 投影曲线起伏最大; 歪着的时候字跨行
/// 涂抹, 曲线被抹平。所以取平方和最大的那个角。
///
/// y 必须逐行走。隔行采样会在投影里长出一条周期等于步长的锯齿, 它的起伏远
/// 大于文字行本身、而且跟角度无关, 分数会整个被它主导 —— 量出来永远是 0 度。
/// 横向倒是可以隔着取, 一行里的墨迹够多。
fn score(g: &Gray, t: f32, step: usize) -> f64 {
    let (w, h) = (g.w, g.h);
    let cx = w as f32 / 2.0;
    let mut prof = vec![0u32; h];
    for y in 0..h {
        let row = &g.px[y * w..(y + 1) * w];
        for x in (0..w).step_by(step) {
            if row[x] < INK {
                let yy = y as f32 + (x as f32 - cx) * t;
                if yy >= 0.0 {
                    let yy = yy as usize;
                    if yy < h {
                        prof[yy] += 1;
                    }
                }
            }
        }
    }
    prof.windows(2)
        .map(|p| {
            let d = p[1] as f64 - p[0] as f64;
            d * d
        })
        .sum()
}

/// 在 [lo, hi] 里按 step 找分数最高的角度
///
/// 起手就把 0 度的分数放进去当擂主: 平白无奇的页(整页空白、只有一个印章)
/// 各个角度分数一样, 谁先来谁当选 —— 那会让扫描起点 -3 度莫名其妙地胜出。
/// 以 0 度为基准, 没有明显更好的角度就不动。
fn best_in(g: &Gray, lo: f32, hi: f32, step: f32, sample: usize) -> f32 {
    let mut best = (score(g, 0.0, sample), 0.0f32);
    let mut a = lo;
    while a <= hi + 1e-6 {
        let s = score(g, a.to_radians().tan(), sample);
        if s > best.0 {
            best = (s, a);
        }
        a += step;
    }
    best.1
}

/// 这一页歪了多少度
///
/// 先粗后细: 全量扫 0.1 度一档要跑六十趟投影, 而 0.4 度一档先圈出大概位置,
/// 再在附近细扫, 两趟加起来只要二十来趟, 结果一样。
pub fn angle(g: &Gray) -> f32 {
    if g.w == 0 || g.h == 0 {
        return 0.0;
    }
    // 横向取两百来个采样点就够定角, 再密下去只是多花时间
    let sample = (g.w / 240).max(1);
    let coarse = best_in(g, -MAX_DEG, MAX_DEG, 0.4, sample);
    // 细扫的窗口要夹回量程里, 否则粗扫落在端点时会一路探到 -3.4 度去
    let (lo, hi) = ((coarse - 0.4).max(-MAX_DEG), (coarse + 0.4).min(MAX_DEG));
    best_in(g, lo, hi, 0.1, sample)
}

/// 绕图心转 `deg` 度; 转出边界的地方填白
///
/// 反向映射 + 双线性: 正向映射会在目标图上留下没被写到的空洞。空出来的角落
/// 填白而不是填黑 —— 填黑会被后面当成一大片墨迹, 框线检测和噪声过滤都会中招。
pub fn apply(g: &Gray, deg: f32) -> Gray {
    let (w, h) = (g.w, g.h);
    let (sin, cos) = deg.to_radians().sin_cos();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let mut px = vec![255u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            let sx = cos * dx - sin * dy + cx;
            let sy = sin * dx + cos * dy + cy;
            if sx < 0.0 || sy < 0.0 {
                continue;
            }
            let (x0, y0) = (sx as usize, sy as usize);
            if x0 + 1 >= w || y0 + 1 >= h {
                continue;
            }
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let i = y0 * w + x0;
            let top = g.px[i] as f32 * (1.0 - fx) + g.px[i + 1] as f32 * fx;
            let bot = g.px[i + w] as f32 * (1.0 - fx) + g.px[i + w + 1] as f32 * fx;
            px[y * w + x] = (top * (1.0 - fy) + bot * fy) as u8;
        }
    }
    Gray { w, h, px }
}

/// 量一下, 歪得够多就转正; 返回量到的歪斜角
///
/// [`angle`] 报的是"内容现在歪了多少", 要摆正当然是往反方向转。
pub fn straighten(g: &mut Gray) -> f32 {
    let a = angle(g);
    if a.abs() < MIN_DEG {
        return 0.0;
    }
    *g = apply(g, -a);
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一页排了 20 行字的样子: 每行一条粗横带
    fn lined_page() -> Gray {
        let (w, h) = (600, 800);
        let mut px = vec![255u8; w * h];
        for r in 0..20 {
            let y0 = 30 + r * 38;
            for y in y0..y0 + 12 {
                for x in 60..540 {
                    px[y * w + x] = 20;
                }
            }
        }
        Gray { w, h, px }
    }

    #[test]
    fn a_straight_page_measures_flat() {
        assert!(angle(&lined_page()).abs() < MIN_DEG, "本来就是正的");
    }

    /// 转过已知的角度, 得量得回来 —— 这条不成立, 后面所有结论都是空的
    #[test]
    fn measures_back_a_known_rotation() {
        for truth in [0.8f32, -1.2, 2.0] {
            let got = angle(&apply(&lined_page(), truth));
            assert!(
                (got - truth).abs() < 0.2,
                "转了 {truth} 度, 量得 {got} —— 报的就是内容现在歪多少"
            );
        }
    }

    #[test]
    fn straightens_a_tilted_page_back_to_flat() {
        let mut g = apply(&lined_page(), 1.1);
        let turned = straighten(&mut g);
        assert!(turned.abs() > MIN_DEG, "该转的没转");
        assert!(angle(&g).abs() < MIN_DEG, "转完还是歪的");
    }

    #[test]
    fn leaves_a_nearly_straight_page_alone() {
        let mut g = apply(&lined_page(), 0.05);
        assert_eq!(straighten(&mut g), 0.0, "这点角度不值得重采样一遍");
    }

    /// 转出边界的角落要填白: 填黑会被后面当成一大片墨迹
    #[test]
    fn fills_the_corners_with_white() {
        let g = apply(&lined_page(), 2.0);
        assert_eq!(g.px[0], 255, "左上角是转空出来的");
        assert_eq!(g.px[g.w - 1], 255);
    }

    #[test]
    fn survives_a_blank_page() {
        let g = Gray {
            w: 40,
            h: 40,
            px: vec![255; 1600],
        };
        assert_eq!(angle(&g), 0.0, "全白页没有峰可找, 就该按不歪算");
        let mut g2 = Gray {
            w: 0,
            h: 0,
            px: Vec::new(),
        };
        assert_eq!(straighten(&mut g2), 0.0);
    }
}
