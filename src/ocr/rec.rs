//! 文字识别 (CRNN + CTC)
//!
//! 批内按宽高比排序再切批, 跟 RapidOCR 一样 —— 同一批里最宽的那张决定整批的
//! 输入宽度, 宽窄混在一起会让窄图补一大片空白, 白算。

use anyhow::Result;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;

use crate::imgutil::Gray;
use crate::ocr::oe;

const IMG_H: usize = 48;
const IMG_W: usize = 320;
const BATCH: usize = 6;

/// 复核时横向拉宽的倍数
///
/// 模型输入高度写死 48(给 64 直接报维度不符), 想让 CTC 多出几帧只能拉宽度。
/// 1.5 是实测出来的: Conclusion-for-QA 那处 iii 在 1.5 倍下认对了, 2.0 倍反而
/// 又认回 ii, 还把 "i. BookTicker" 的空格吞了 —— 拉太狠字形就跑出模型见过的
/// 比例了。
const RECHECK_K: f32 = 1.5;

/// 认一遍
pub fn recognize(
    sess: &mut Session,
    crops: &[Gray],
    charset: &[String],
) -> Result<Vec<(String, f32)>> {
    let all: Vec<usize> = (0..crops.len()).collect();
    run(sess, crops, &all, 1.0, charset)
}

/// 挑几张拉宽了再认一遍, 结果按 pick 的顺序返回
pub fn recheck(
    sess: &mut Session,
    crops: &[Gray],
    pick: &[usize],
    charset: &[String],
) -> Result<Vec<(String, f32)>> {
    run(sess, crops, pick, RECHECK_K, charset)
}

/// `k` 是横向拉伸倍数, 1.0 就是保持原始宽高比
fn run(
    sess: &mut Session,
    crops: &[Gray],
    pick: &[usize],
    k: f32,
    charset: &[String],
) -> Result<Vec<(String, f32)>> {
    let mut out = vec![(String::new(), 0.0f32); pick.len()];
    if pick.is_empty() {
        return Ok(out);
    }
    // 按宽高比排序, 相近的分到一批
    let mut idx: Vec<usize> = (0..pick.len()).collect();
    idx.sort_by(|&a, &b| {
        let ra = crops[pick[a]].w as f32 / crops[pick[a]].h as f32;
        let rb = crops[pick[b]].w as f32 / crops[pick[b]].h as f32;
        ra.partial_cmp(&rb).unwrap()
    });

    for chunk in idx.chunks(BATCH) {
        let ratio_of = |i: usize| k * crops[pick[i]].w as f32 / crops[pick[i]].h as f32;
        let mut max_ratio = IMG_W as f32 / IMG_H as f32;
        for &i in chunk {
            max_ratio = max_ratio.max(ratio_of(i));
        }
        let bw = (IMG_H as f32 * max_ratio) as usize;
        let mut input = Array4::<f32>::zeros((chunk.len(), 3, IMG_H, bw));
        for (n, &i) in chunk.iter().enumerate() {
            let g = &crops[pick[i]];
            let rw = ((IMG_H as f32 * ratio_of(i)).ceil() as usize)
                .min(bw)
                .max(1);
            let small = super::resize(g, rw, IMG_H);
            for y in 0..IMG_H {
                for x in 0..rw {
                    let v = (small.at(x, y) as f32 / 255.0 - 0.5) / 0.5;
                    for c in 0..3 {
                        input[[n, c, y, x]] = v;
                    }
                }
            }
            // 右边补 0 —— padding_im 本来就是零, 不用再写
        }

        let tensor = Tensor::from_array(input).map_err(oe)?;
        let outputs = sess.run(ort::inputs!["x" => tensor]).map_err(oe)?;
        let (shape, data) = outputs[0].try_extract_tensor::<f32>().map_err(oe)?;
        let (t_len, n_cls) = (shape[1] as usize, shape[2] as usize);
        for (n, &i) in chunk.iter().enumerate() {
            out[i] = ctc_decode(
                &data[n * t_len * n_cls..(n + 1) * t_len * n_cls],
                t_len,
                n_cls,
                charset,
            );
        }
    }
    Ok(out)
}

/// CTC 贪心解码: 逐帧取最大, 去重复、去 blank(索引 0), 置信度取所选帧的均值
fn ctc_decode(logits: &[f32], t_len: usize, n_cls: usize, charset: &[String]) -> (String, f32) {
    let mut text = String::new();
    let mut confs: Vec<f32> = Vec::new();
    let mut prev = usize::MAX;
    for t in 0..t_len {
        let row = &logits[t * n_cls..(t + 1) * n_cls];
        let mut best = 0usize;
        let mut bv = row[0];
        for (c, &v) in row.iter().enumerate().skip(1) {
            if v > bv {
                bv = v;
                best = c;
            }
        }
        let dup = best == prev;
        prev = best;
        if dup || best == 0 {
            continue;
        }
        if let Some(ch) = charset.get(best) {
            text.push_str(ch);
            confs.push(bv);
        }
    }
    let s = if confs.is_empty() {
        0.0
    } else {
        confs.iter().sum::<f32>() / confs.len() as f32
    };
    (text, s)
}
