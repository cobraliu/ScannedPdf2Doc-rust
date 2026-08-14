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
use std::path::{Path, PathBuf};

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

/// 建 session 时的取舍开关
///
/// 这里每一项都只改"怎么算", 不改"算什么": 喂进去的张量、走的算子、算子的
/// 实现全都一模一样, CPU 上又是确定性的, 所以任何组合下识别结果都逐字相同,
/// 变的只有耗时和峰值内存。手机上内存比速度金贵, 这几个开关就是拿时间换内存的。
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// 单个算子内部的线程数; None = 跟可用核心数走
    pub intra_threads: Option<usize>,
    /// ORT 的 CPU 内存池。开着快, 但它只涨不还 —— 峰值按历史最高水位算,
    /// 而不是按当下真正在用的量
    pub arena: bool,
    /// 按首次运行的形状预分配一整块。我们每页每批的形状都在变, 这块预分配
    /// 未必用得上, 但内存照占
    pub memory_pattern: bool,
    /// 三个 session 轮流上场, 用完立刻放掉。run() 本来就是 det -> cls -> rec
    /// 一段段做完的, 谁也不需要跟别人同时在场, 所以这么改不影响结果
    pub lazy: bool,
    /// 要求 ORT 走确定性的算子实现
    ///
    /// 不开的话同一个二进制、同一张图, 跑两次可能给出不一样的结果: 多线程下
    /// 归约的累加顺序不固定, 浮点最后一位对不上, 框的坐标就可能差一个像素,
    /// 版面重建那边跟着把缩进段判成项目符号列表。
    pub deterministic: bool,
    /// 只给检测这一步的输入封长边; None = 不封(整页多大就多大)
    ///
    /// **这一项跟上面三个不一样, 它会改结果。** 峰值内存几乎全花在检测上,
    /// 而且随检测输入的面积走, 所以这是唯一还能大幅往下压的地方。裁剪和识别
    /// 用的仍然是整页原图, 认字的清晰度不受影响; 受影响的是"框画得准不准",
    /// 极端情况下小字会漏检。定成多少必须拿实拍件测过再说。
    pub det_max_side: Option<f32>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            intra_threads: None,
            arena: true,
            memory_pattern: true,
            lazy: false,
            deterministic: false,
            det_max_side: None,
        }
    }
}

impl EngineOptions {
    /// 手机上的推荐组合 —— 实测出来的, 不是拍脑袋:
    /// 只开 lazy。峰值 694 -> 567 MB, 识别 0.45 -> 0.47s, 识别结果逐字不变。
    ///
    /// 另外三个开关实测都不该开:
    /// - arena 关掉反而更差(567 -> 628 MB)。lazy 下同时只有一个 session 在场,
    ///   它那个池子本来就是按需长出来的; 关掉之后每块都单独 malloc, 归还不及时,
    ///   RSS 的高水位反倒更高
    /// - memory_pattern 关不关都是 567 MB, 没有区别
    /// - 线程从 10 降到 2 只省 1 MB, 却慢 1.7 倍 —— 内存根本不花在线程上
    pub fn low_memory() -> Self {
        Self {
            lazy: true,
            ..Self::default()
        }
    }
}

/// 三个模型各自的文件名, 顺带当 lazy 模式下的槽位下标
#[derive(Clone, Copy, PartialEq, Eq)]
enum Which {
    Det,
    Cls,
    Rec,
}

impl Which {
    fn file(self) -> &'static str {
        match self {
            Which::Det => "PP-OCRv6_det_small.onnx",
            Which::Cls => "ch_ppocr_mobile_v2.0_cls_mobile.onnx",
            Which::Rec => "PP-OCRv6_rec_small.onnx",
        }
    }
}

pub struct Engine {
    dir: PathBuf,
    opts: EngineOptions,
    det: Option<Session>,
    cls: Option<Session>,
    rec: Option<Session>,
    /// CTC 字典: ["blank"] + 模型内嵌字表 + [" "]
    charset: Vec<String>,
}

/// 单页最长边 —— 超过就先缩下来再识别, 与 RapidOCR 的 max_side_len 一致
const MAX_SIDE: f32 = 2000.0;
const MIN_SIDE: f32 = 30.0;

impl Engine {
    pub fn load(model_dir: &Path) -> Result<Self> {
        Self::load_with(model_dir, EngineOptions::default())
    }

    pub fn load_with(model_dir: &Path, opts: EngineOptions) -> Result<Self> {
        let mut e = Self {
            dir: model_dir.to_path_buf(),
            opts,
            det: None,
            cls: None,
            rec: None,
            charset: Vec::new(),
        };

        // 保持原来的 det -> cls -> rec 顺序。实测顺序本身不影响结果, 只是没有
        // 理由为了读个字表就把 rec 提前 —— 少一个变量, 以后排查省事。
        if !e.opts.lazy {
            e.det = Some(e.build(Which::Det)?);
            e.cls = Some(e.build(Which::Cls)?);
        }
        // 字表内嵌在 rec 模型的 metadata 里(PP-OCRv6 起是这样), 不用另带字典文件
        let rec = e.build(Which::Rec)?;
        e.charset = {
            let meta = rec.metadata().map_err(oe)?;
            let raw = meta
                .custom("character")
                .context("rec 模型里没有 character 元数据")?;
            let mut cs: Vec<String> = vec!["blank".into()];
            cs.extend(raw.lines().map(|s| s.trim_end_matches('\r').to_string()));
            cs.push(" ".into());
            cs
        };
        // lazy 模式下这个 rec 就地丢掉, 等真要识别时再建 —— 多花一次加载,
        // 换的是"任何时刻只有一个 session 在场"
        if !e.opts.lazy {
            e.rec = Some(rec);
        }
        Ok(e)
    }

    fn build(&self, w: Which) -> Result<Session> {
        let threads = self.opts.intra_threads.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        });
        let p = self.dir.join(w.file());
        let mut b = Session::builder()
            .map_err(oe)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(oe)?
            .with_intra_threads(threads)
            .map_err(oe)?
            .with_memory_pattern(self.opts.memory_pattern)
            .map_err(oe)?
            .with_deterministic_compute(self.opts.deterministic)
            .map_err(oe)?;
        if !self.opts.arena {
            // arena 的开关挂在 EP 上, 不在 SessionBuilder 上 —— 显式注册一个
            // 关掉 arena 的 CPU EP 才能覆盖掉默认那个
            b = b
                .with_execution_providers([ort::ep::CPU::default()
                    .with_arena_allocator(false)
                    .build()])
                .map_err(oe)?;
        }
        b.commit_from_file(&p)
            .map_err(oe)
            .with_context(|| format!("加载模型失败: {}", p.display()))
    }

    /// 取出某个 session 用; lazy 模式下现建
    fn take(&mut self, w: Which) -> Result<Session> {
        let slot = match w {
            Which::Det => &mut self.det,
            Which::Cls => &mut self.cls,
            Which::Rec => &mut self.rec,
        };
        match slot.take() {
            Some(s) => Ok(s),
            None => self.build(w),
        }
    }

    /// 用完还回去; lazy 模式下直接丢掉, 连同它那份 arena
    fn give_back(&mut self, w: Which, s: Session) {
        if self.opts.lazy {
            drop(s);
            return;
        }
        match w {
            Which::Det => self.det = Some(s),
            Which::Cls => self.cls = Some(s),
            Which::Rec => self.rec = Some(s),
        }
    }

    /// 识别一页, 返回按"先上后下、再左右"排好的文字块
    pub fn run(&mut self, page: &Gray) -> Result<Vec<Item>> {
        // ---- 0. 整页缩放: 太大先缩(省一半时间), 太小放大 ----
        let (img, ratio_w, ratio_h) = fit_bounds(page);

        // ---- 1. 检测 ----
        let mut s = self.take(Which::Det)?;
        let quads = det::detect(&mut s, &img, self.opts.det_max_side);
        self.give_back(Which::Det, s);
        let quads = quads?;
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
        let mut s = self.take(Which::Cls)?;
        let r = cls::classify_and_rotate(&mut s, &mut crops);
        self.give_back(Which::Cls, s);
        r?;

        // ---- 4. 识别 ----
        let mut s = self.take(Which::Rec)?;
        let texts = rec::recognize(&mut s, &crops, &self.charset);
        self.give_back(Which::Rec, s);
        let texts = texts?;

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
            Gray {
                w: src.w,
                h: src.h,
                px: src.px.clone(),
            },
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
