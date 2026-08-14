//! DB 文字检测 —— 前后处理照搬 PaddleOCR/RapidOCR 的参数
//!
//! 后处理这一步 Python 版用了三个库: cv2.findContours + cv2.minAreaRect 找框、
//! pyclipper 外扩、shapely 算面积。这里用连通域 + 凸包 + 旋转卡壳 + 四边形偏移
//! 替掉, 对"文字块"这种凸性很好的形状结果一致。

use anyhow::Result;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;

use crate::geom::{self, Pt};
use crate::imgutil::{components_with_points, Bitmap};
use crate::ocr::{oe, Gray};

const LIMIT_SIDE_LEN: f32 = 736.0; // limit_type = min
const THRESH: f32 = 0.3;
const BOX_THRESH: f32 = 0.5;
const UNCLIP_RATIO: f32 = 1.6;
const MIN_SIZE: f32 = 3.0;
const MAX_CANDIDATES: usize = 1000;
/// 行内排序时, y 差小于这个值算同一行
const SORT_Y_TOL: f32 = 10.0;

pub fn detect(sess: &mut Session, img: &Gray) -> Result<Vec<[Pt; 4]>> {
    // ---- 预处理: 短边不足 736 就放大, 再取 32 的整数倍 ----
    let (h, w) = (img.h as f32, img.w as f32);
    let ratio = if h.min(w) < LIMIT_SIDE_LEN {
        LIMIT_SIDE_LEN / h.min(w)
    } else {
        1.0
    };
    let rh = (((h * ratio) / 32.0).round() * 32.0).max(32.0) as usize;
    let rw = (((w * ratio) / 32.0).round() * 32.0).max(32.0) as usize;
    let small = if rh == img.h && rw == img.w {
        Gray { w: img.w, h: img.h, px: img.px.clone() }
    } else {
        super::resize(img, rw, rh)
    };

    // (v/255 - 0.5) / 0.5, 三个通道同值(扫描件本来就是灰的)
    let mut input = Array4::<f32>::zeros((1, 3, rh, rw));
    for y in 0..rh {
        for x in 0..rw {
            let v = small.at(x, y) as f32 / 255.0;
            let n = (v - 0.5) / 0.5;
            input[[0, 0, y, x]] = n;
            input[[0, 1, y, x]] = n;
            input[[0, 2, y, x]] = n;
        }
    }

    let tensor = Tensor::from_array(input).map_err(oe)?;
    let outputs = sess.run(ort::inputs!["x" => tensor]).map_err(oe)?;
    let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(oe)?;
    let (ph, pw) = (shape[2] as usize, shape[3] as usize);

    // ---- 后处理 ----
    let mut mask = Bitmap::new(pw, ph);
    for y in 0..ph {
        for x in 0..pw {
            if data[y * pw + x] > THRESH {
                mask.set(x, y, 1);
            }
        }
    }
    let mask = dilate2x2(&mask);

    let mut boxes: Vec<([Pt; 4], f32)> = Vec::new();
    for (_, pts) in components_with_points(&mask).into_iter().take(MAX_CANDIDATES) {
        let hull = geom::convex_hull(&pts);
        let quad = geom::min_area_rect(&hull);
        if short_side(&quad) < MIN_SIZE {
            continue;
        }
        let score = box_score_fast(data, pw, ph, &quad);
        if score < BOX_THRESH {
            continue;
        }
        let grown = geom::unclip_quad(geom::order_quad(quad), UNCLIP_RATIO);
        let quad = geom::min_area_rect(&geom::convex_hull(&to_i32(&grown)));
        if short_side(&quad) < MIN_SIZE + 2.0 {
            continue;
        }
        // 映射回缩放前的整页坐标
        let sx = img.w as f32 / pw as f32;
        let sy = img.h as f32 / ph as f32;
        let mut q = geom::order_quad(quad);
        for p in q.iter_mut() {
            p.0 = (p.0 * sx).round().clamp(0.0, img.w as f32);
            p.1 = (p.1 * sy).round().clamp(0.0, img.h as f32);
        }
        let wid = dist(q[0], q[1]);
        let hei = dist(q[0], q[3]);
        if wid <= 3.0 || hei <= 3.0 {
            continue;
        }
        boxes.push((q, score));
    }

    // ---- 排序: 先按 y 分行, 行内按 x ----
    boxes.sort_by(|a, b| a.0[0].1.partial_cmp(&b.0[0].1).unwrap());
    let mut line_id = Vec::with_capacity(boxes.len());
    let mut cur = 0usize;
    for i in 0..boxes.len() {
        if i > 0 && boxes[i].0[0].1 - boxes[i - 1].0[0].1 >= SORT_Y_TOL {
            cur += 1;
        }
        line_id.push(cur);
    }
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|&a, &b| {
        line_id[a]
            .cmp(&line_id[b])
            .then(boxes[a].0[0].0.partial_cmp(&boxes[b].0[0].0).unwrap())
    });
    Ok(order.into_iter().map(|i| boxes[i].0).collect())
}

/// cv2.dilate 用 2x2 核、锚点在 (0,0): out(x,y) = max of in(x..x+1, y..y+1)
fn dilate2x2(bm: &Bitmap) -> Bitmap {
    let mut out = Bitmap::new(bm.w, bm.h);
    for y in 0..bm.h {
        for x in 0..bm.w {
            let mut v = 0u8;
            for dy in 0..2 {
                for dx in 0..2 {
                    if x + dx < bm.w && y + dy < bm.h {
                        v |= bm.at(x + dx, y + dy);
                    }
                }
            }
            out.set(x, y, v);
        }
    }
    out
}

fn short_side(q: &[Pt; 4]) -> f32 {
    let a = dist(q[0], q[1]);
    let b = dist(q[1], q[2]);
    a.min(b)
}

fn dist(a: Pt, b: Pt) -> f32 {
    ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt()
}

fn to_i32(q: &[Pt; 4]) -> Vec<(i32, i32)> {
    q.iter().map(|&(x, y)| (x.round() as i32, y.round() as i32)).collect()
}

/// 框内概率均值 —— 对应 box_score_fast
fn box_score_fast(pred: &[f32], w: usize, h: usize, q: &[Pt; 4]) -> f32 {
    let (x0, y0, x1, y1) = geom::bounds(q);
    let xmin = (x0.floor().max(0.0) as usize).min(w - 1);
    let xmax = (x1.ceil().max(0.0) as usize).min(w - 1);
    let ymin = (y0.floor().max(0.0) as usize).min(h - 1);
    let ymax = (y1.ceil().max(0.0) as usize).min(h - 1);
    let (mut sum, mut n) = (0.0f32, 0usize);
    for y in ymin..=ymax {
        for x in xmin..=xmax {
            if inside(q, x as f32 + 0.5, y as f32 + 0.5) {
                sum += pred[y * w + x];
                n += 1;
            }
        }
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f32
    }
}

fn inside(q: &[Pt; 4], px: f32, py: f32) -> bool {
    let mut hit = false;
    let mut j = 3;
    for i in 0..4 {
        let (xi, yi) = q[i];
        let (xj, yj) = q[j];
        if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            hit = !hit;
        }
        j = i;
    }
    hit
}
