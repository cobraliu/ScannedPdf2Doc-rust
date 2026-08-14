//! OCR: PP-OCRv6 检测 + 方向分类 + 识别
//!
//! 模型和参数跟 Python 版(RapidOCR)完全一致 —— 同样的 3 个 .onnx, 同样的
//! limit_side_len / thresh / unclip_ratio。区别只在前后处理是手写的:
//! RapidOCR 的前后处理靠 opencv + pyclipper + shapely, 那三个加起来 130 MB。

mod cls;
mod det;
mod rec;

use anyhow::{anyhow, Context, Result};
use ort::session::{builder::GraphOptimizationLevel, Session};
use std::path::Path;

use crate::imgutil::Gray;

/// ort 的错误里含裸指针, 不是 Send + Sync, anyhow 收不下 —— 只留消息
pub(crate) fn oe<R>(e: ort::Error<R>) -> anyhow::Error {
    anyhow!("ONNX Runtime: {e}")
}

/// 一个识别结果: 文字 + 外接框 + 置信度
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Item {
    pub t: String,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// 置信度
    pub s: f32,
}

pub struct Engine {
    det: Session,
    cls: Session,
    rec: Session,
    /// CTC 字典: ["blank"] + 模型内嵌字表 + [" "]
    charset: Vec<String>,
}

/// 单页最长边 —— 超过就先缩下来再识别, 与 RapidOCR 的 max_side_len 一致
const MAX_SIDE: f32 = 2000.0;
const MIN_SIDE: f32 = 30.0;

impl Engine {
    pub fn load(model_dir: &Path) -> Result<Self> {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let mk = |name: &str| -> Result<Session> {
            let p = model_dir.join(name);
            let mut b = Session::builder()
                .map_err(oe)?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(oe)?
                .with_intra_threads(threads)
                .map_err(oe)?;
            b.commit_from_file(&p)
                .map_err(oe)
                .with_context(|| format!("加载模型失败: {}", p.display()))
        };
        let det = mk("PP-OCRv6_det_small.onnx")?;
        let cls = mk("ch_ppocr_mobile_v2.0_cls_mobile.onnx")?;
        let rec = mk("PP-OCRv6_rec_small.onnx")?;

        // 字表内嵌在 rec 模型的 metadata 里(PP-OCRv6 起是这样), 不用另带字典文件。
        // 单独开个块: metadata() 借着 rec, 不放掉就没法把 rec 移进 Self
        let charset = {
            let meta = rec.metadata().map_err(oe)?;
            let raw = meta
                .custom("character")
                .context("rec 模型里没有 character 元数据")?;
            let mut cs: Vec<String> = vec!["blank".into()];
            cs.extend(raw.lines().map(|s| s.trim_end_matches('\r').to_string()));
            cs.push(" ".into());
            cs
        };

        Ok(Self { det, cls, rec, charset })
    }

    /// 识别一页, 返回按"先上后下、再左右"排好的文字块
    pub fn run(&mut self, page: &Gray) -> Result<Vec<Item>> {
        // ---- 0. 整页缩放: 太大先缩(省一半时间), 太小放大 ----
        let (img, ratio_w, ratio_h) = fit_bounds(page);

        // ---- 1. 检测 ----
        let quads = det::detect(&mut self.det, &img)?;
        if quads.is_empty() {
            return Ok(Vec::new());
        }

        // ---- 2. 逐框裁出小图, 竖排的转正 ----
        let mut crops = Vec::with_capacity(quads.len());
        for q in &quads {
            let (w, h, px) = crate::geom::crop_quad(&img, *q);
            let mut g = Gray { w, h, px };
            // 高宽比 >= 1.5 认为是竖排, 转 90 度 —— 与 get_rotate_crop_image 一致
            if h as f32 / w.max(1) as f32 >= 1.5 {
                g = rot90(&g);
            }
            crops.push(g);
        }

        // ---- 3. 方向分类: 倒着的转回来 ----
        cls::classify_and_rotate(&mut self.cls, &mut crops)?;

        // ---- 4. 识别 ----
        let texts = rec::recognize(&mut self.rec, &crops, &self.charset)?;

        // ---- 5. 组装: 框映射回原图坐标 ----
        let mut out = Vec::new();
        for (q, (t, s)) in quads.iter().zip(texts) {
            if t.is_empty() || s < 0.5 {
                continue; // text_score: 0.5
            }
            let (x0, y0, x1, y1) = crate::geom::bounds(q);
            out.push(Item {
                t,
                x0: x0 * ratio_w,
                y0: y0 * ratio_h,
                x1: x1 * ratio_w,
                y1: y1 * ratio_h,
                s,
            });
        }
        Ok(out)
    }
}

/// 整页缩放到 [MIN_SIDE, MAX_SIDE], 返回缩放后的图与"还原回原尺寸"的比例
fn fit_bounds(src: &Gray) -> (Gray, f32, f32) {
    let (h, w) = (src.h as f32, src.w as f32);
    let mut ratio = 1.0f32;
    if h.max(w) > MAX_SIDE {
        ratio = MAX_SIDE / h.max(w);
    } else if h.min(w) < MIN_SIDE {
        ratio = MIN_SIDE / h.min(w);
    }
    if (ratio - 1.0).abs() < 1e-6 {
        return (
            Gray { w: src.w, h: src.h, px: src.px.clone() },
            1.0,
            1.0,
        );
    }
    // 边长取 32 的整数倍, 跟 RapidOCR 一样
    let rw = (((w * ratio) / 32.0).round() * 32.0).max(32.0) as usize;
    let rh = (((h * ratio) / 32.0).round() * 32.0).max(32.0) as usize;
    (resize(src, rw, rh), w / rw as f32, h / rh as f32)
}

/// 双线性缩放
pub fn resize(src: &Gray, nw: usize, nh: usize) -> Gray {
    let mut px = vec![0u8; nw * nh];
    let sx = src.w as f32 / nw as f32;
    let sy = src.h as f32 / nh as f32;
    for y in 0..nh {
        let fy = ((y as f32 + 0.5) * sy - 0.5).max(0.0);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(src.h - 1);
        let wy = fy - y0 as f32;
        for x in 0..nw {
            let fx = ((x as f32 + 0.5) * sx - 0.5).max(0.0);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(src.w - 1);
            let wx = fx - x0 as f32;
            let v = src.at(x0, y0) as f32 * (1.0 - wx) * (1.0 - wy)
                + src.at(x1, y0) as f32 * wx * (1.0 - wy)
                + src.at(x0, y1) as f32 * (1.0 - wx) * wy
                + src.at(x1, y1) as f32 * wx * wy;
            px[y * nw + x] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    Gray { w: nw, h: nh, px }
}

/// 逆时针转 90 度(np.rot90)
fn rot90(g: &Gray) -> Gray {
    let (w, h) = (g.h, g.w);
    let mut px = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            // rot90: out[y][x] = in[x][W-1-y]; at() 收的是 (列, 行)
            px[y * w + x] = g.at(g.w - 1 - y, x);
        }
    }
    Gray { w, h, px }
}

pub fn rot180(g: &Gray) -> Gray {
    let mut px = g.px.clone();
    px.reverse();
    Gray { w: g.w, h: g.h, px }
}
