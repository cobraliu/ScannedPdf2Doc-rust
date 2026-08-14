//! 方向分类: 判断裁出来的小图是不是倒着的(180 度)
//!
//! 扫描件里被盖章、装订压歪的行偶尔会整块倒过来; 不转正的话识别出来是乱码。

use anyhow::Result;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;

use crate::imgutil::Gray;
use crate::ocr::oe;

const IMG_H: usize = 48;
const IMG_W: usize = 192;
const BATCH: usize = 6;
const THRESH: f32 = 0.9;

pub fn classify_and_rotate(sess: &mut Session, crops: &mut [Gray]) -> Result<()> {
    for chunk_start in (0..crops.len()).step_by(BATCH) {
        let end = (chunk_start + BATCH).min(crops.len());
        let n = end - chunk_start;
        let mut input = Array4::<f32>::zeros((n, 3, IMG_H, IMG_W));
        for (k, g) in crops[chunk_start..end].iter().enumerate() {
            let ratio = g.w as f32 / g.h as f32;
            let rw = ((IMG_H as f32 * ratio).ceil() as usize).clamp(1, IMG_W);
            let small = super::resize(g, rw, IMG_H);
            for y in 0..IMG_H {
                for x in 0..rw {
                    let v = (small.at(x, y) as f32 / 255.0 - 0.5) / 0.5;
                    for c in 0..3 {
                        input[[k, c, y, x]] = v;
                    }
                }
            }
        }
        let tensor = Tensor::from_array(input).map_err(oe)?;
        let outputs = sess.run(ort::inputs!["x" => tensor]).map_err(oe)?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(oe)?;
        let ncls = shape[1] as usize;
        for k in 0..n {
            let p0 = data[k * ncls];
            let p1 = data[k * ncls + 1];
            // label_list = ['0', '180'], 第二类是倒着的
            if p1 > p0 && p1 > THRESH {
                crops[chunk_start + k] = super::rot180(&crops[chunk_start + k]);
            }
        }
    }
    Ok(())
}
