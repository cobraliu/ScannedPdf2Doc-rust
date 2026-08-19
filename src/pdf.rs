//! PDF 渲染与文本层 —— pdfium
//!
//! Python 版用 PyMuPDF(整包 46 MB, 光 libmupdf.dylib 就 32 MB)。pdfium 的
//! 动态库约 7 MB, 渲染质量与速度都够, 是这次换栈能省下来的第二大头。
//!
//! 逐页渲染而不是一次全渲染: 300 dpi 的 A4 灰度图一页 8.7 MB, 一份 96 页的
//! 合同全存内存就是 800 MB。Python 版靠落盘缓存绕开, 这里直接流式处理。

use anyhow::{anyhow, Context, Result};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};

use crate::imgutil::{Gray, Rgb};

/// 竖线判定(页图像素): 短于这个的是标点、下划线碎片
const RULE_MIN: f32 = 10.0;
/// 宽于这个的是色块不是线 —— 实测框线是 2 个点, 300 dpi 下约 8 px
const RULE_MAX: f32 = 14.0;

/// 文本层里的一个字符, 坐标已经换算成页图的像素坐标
#[derive(Debug, Clone, Copy)]
pub struct Ch {
    pub c: char,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    /// 字号, 同样换算成像素 —— 行内该不该断开拿它当尺子
    pub size: f32,
}

/// 页面上画出来的一条竖线, 坐标同样是页图像素
#[derive(Debug, Clone, Copy)]
pub struct VRule {
    pub x: f32,
    pub y0: f32,
    pub y1: f32,
}

pub struct Renderer {
    pdfium: Pdfium,
}

/// 打开着的一份 PDF
pub struct Pages<'a> {
    doc: PdfDocument<'a>,
    long_edge: u32,
}

impl Renderer {
    /// 绑定 pdfium
    ///
    /// pdfium 的绑定在 pdfium-render 里是**进程级全局**的(一个 OnceCell), 第二次
    /// 调 bind_to_library 一律返回 AlreadyInitialized —— 它连文件都不去看。所以
    /// 这个错不是失败, 是"已经有人绑过了", 得认下来复用: GUI 里转完一批再转一批
    /// 会新建 Converter, 不认这个错第二次就起不来。
    ///
    /// 复用走 Pdfium::default(): 它内部对 AlreadyInitialized 正是走复用分支
    /// (见 pdfium-render 的 impl Default for Pdfium)。只在已初始化时调它才安全
    /// —— 未初始化时它会去找 ./libpdfium 并可能 panic。
    pub fn new() -> Result<Self> {
        let lib = locate_pdfium()?;
        let pdfium = match Pdfium::bind_to_library(&lib) {
            Ok(b) => Pdfium::new(b),
            Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Pdfium::default(),
            Err(e) => {
                return Err(anyhow!("绑定 pdfium 失败: {}: {e}", lib.display()));
            }
        };
        Ok(Self { pdfium })
    }

    pub fn open<'a>(&'a self, pdf: &Path, long_edge: u32) -> Result<Pages<'a>> {
        let doc = self
            .pdfium
            .load_pdf_from_file(pdf, None)
            .with_context(|| format!("打开 PDF 失败: {}", pdf.display()))?;
        Ok(Pages { doc, long_edge })
    }
}

impl Pages<'_> {
    pub fn len(&self) -> usize {
        self.doc.pages().len() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 原件多数页是横版就出横版 docx —— 横版表格塞进竖版 A4 只能缩成蚂蚁
    pub fn mostly_landscape(&self) -> bool {
        let n = self.len();
        let wide = (0..n)
            .filter(|&i| match self.doc.pages().get(i as PdfPageIndex) {
                Ok(p) => p.width().value > p.height().value,
                Err(_) => false,
            })
            .count();
        wide * 2 > n
    }

    /// 页图像素 ÷ PDF 点
    ///
    /// dpi 按本页尺寸倒推, 让长边落在 long_edge 附近 —— 页面大小不一的卷宗里,
    /// 固定 dpi 会让 A3 图纸渲成八千像素、A5 单据只有一千, 阈值没法通用。
    fn scale(&self, page: &PdfPage) -> f32 {
        let long_pt = page.width().value.max(page.height().value).max(1.0);
        let dpi = (self.long_edge as f32 / long_pt * 72.0)
            .round()
            .clamp(150.0, 300.0);
        dpi / 72.0
    }

    /// 页图像素 ÷ 点, 对外的那份
    ///
    /// 出可搜索 PDF 时要把页图贴回一张同样大小的纸上。直接读 page.width()
    /// 不保险: 带 /Rotate 的页面渲出来是转过的, 宽高对调。拿这个比例去除
    /// 图的实际宽高, 无论转没转都对得上。
    pub fn px_per_pt(&self, i: usize) -> Result<f32> {
        let page = self.doc.pages().get(i as PdfPageIndex)?;
        Ok(self.scale(&page))
    }

    /// 渲染参数 —— 渲染和文本层换算必须共用同一份, 否则字框跟页图对不上
    fn render_cfg(&self, page: &PdfPage) -> PdfRenderConfig {
        PdfRenderConfig::new().scale_page_by_factor(self.scale(page))
    }

    /// 渲染一页成灰度图
    ///
    /// 只要灰度: 后面无论 OCR 还是框线检测都在灰度上做, 早一步转掉能省 2/3 内存。
    pub fn render(&self, i: usize) -> Result<Gray> {
        let page = self.doc.pages().get(i as PdfPageIndex)?;
        let cfg = self.render_cfg(&page);
        let img = page.render_with_config(&cfg)?.as_image()?.into_luma8();
        Ok(Gray {
            w: img.width() as usize,
            h: img.height() as usize,
            px: img.into_raw(),
        })
    }

    /// 这一页文本层里的字符; 没有文本层就是空的
    ///
    /// 原生 PDF(Word/Excel 直接导出的那种)里, 每个字是什么、在哪儿本来就精确
    /// 写着。渲染成图再 OCR 一遍, 是拿已知的 100% 去换识别的九成几, 还要多花
    /// 九成时间 —— 有文本层就该直接拿。
    ///
    /// 坐标走 pdfium 自己的 FPDF_PageToDevice, 吃的是渲染那份同一个 config:
    /// 页面带 /Rotate 的很常见, 文本层坐标是转之前的, 自己拿页高翻 y 会整页错位。
    /// 同一页的彩色版 —— 只在要裁印章/插图时才渲, 用的是同一套渲染参数,
    /// 所以跟灰度那张逐像素对得上
    pub fn render_rgb(&self, i: usize) -> Result<Rgb> {
        let page = self.doc.pages().get(i as PdfPageIndex)?;
        let cfg = self.render_cfg(&page);
        let img = page.render_with_config(&cfg)?.as_image()?.into_rgb8();
        Ok(Rgb {
            w: img.width() as usize,
            h: img.height() as usize,
            px: img.into_raw(),
        })
    }

    pub fn chars(&self, i: usize) -> Result<Vec<Ch>> {
        let page = self.doc.pages().get(i as PdfPageIndex)?;
        let cfg = self.render_cfg(&page);
        let k = self.scale(&page);
        let text = page.text()?;
        let mut out = Vec::new();
        for ch in text.chars().iter() {
            let Some(c) = ch.unicode_char() else { continue };
            // pdfium 会在换行、对齐处补进页面上并不存在的字符, 跟着它走会凭空
            // 多出一片零宽框, 把行内间距算乱
            if c.is_control() || ch.is_generated().unwrap_or(false) {
                continue;
            }
            // loose_bounds 按字体的 ascent/descent 给框, 同一行的字高度一致,
            // 聚行才聚得齐; tight_bounds 是墨迹边界, 空格和标点会塌成一条缝
            let Ok(r) = ch.loose_bounds().or_else(|_| ch.tight_bounds()) else {
                continue;
            };
            // 转两个对角点再取 min/max: 页面转过 90 度时, 左下角会落到右上角去
            let (ax, ay) = page.points_to_pixels(r.left(), r.bottom(), &cfg)?;
            let (bx, by) = page.points_to_pixels(r.right(), r.top(), &cfg)?;
            out.push(Ch {
                c,
                x0: ax.min(bx) as f32,
                y0: ay.min(by) as f32,
                x1: ax.max(bx) as f32,
                y1: ay.max(by) as f32,
                size: ch.scaled_font_size().value * k,
            });
        }
        Ok(out)
    }

    /// 这一页画出来的竖线
    ///
    /// 原生 PDF 的表格线是矢量图形, 坐标精确写在文件里, 不用去图上找。文字层
    /// 那条路非要它不可: 相邻两格的字常常各自把格宽填满、中间一丝空隙都没有
    /// (表头尤其如此), 光看间距切不开, 两列的字会串成一句读不通的话。
    ///
    /// 只挑又细又长的。单元格底纹、整页背景也是 path, 但它们又宽又高。
    pub fn vrules(&self, i: usize) -> Result<Vec<VRule>> {
        let page = self.doc.pages().get(i as PdfPageIndex)?;
        let cfg = self.render_cfg(&page);
        let mut out = Vec::new();
        for o in page.objects().iter() {
            if o.object_type() != PdfPageObjectType::Path {
                continue;
            }
            let Ok(b) = o.bounds() else { continue };
            let (ax, ay) = page.points_to_pixels(b.left(), b.bottom(), &cfg)?;
            let (bx, by) = page.points_to_pixels(b.right(), b.top(), &cfg)?;
            let (x0, x1) = (ax.min(bx) as f32, ax.max(bx) as f32);
            let (y0, y1) = (ay.min(by) as f32, ay.max(by) as f32);
            // 细长比在页图上判, 不在 PDF 坐标系判 —— 页面带 /Rotate 时,
            // 文件里的竖线渲染出来是横的
            let (w, h) = (x1 - x0, y1 - y0);
            if h < RULE_MIN || w > RULE_MAX || w > 0.25 * h {
                continue;
            }
            out.push(VRule {
                x: (x0 + x1) / 2.0,
                y0,
                y1,
            });
        }
        Ok(out)
    }
}

/// 找 libpdfium: 环境变量 -> 可执行文件同目录 -> vendor/ -> 系统库
fn locate_pdfium() -> Result<PathBuf> {
    let name = Pdfium::pdfium_platform_library_name();
    let shown = name.to_string_lossy().to_string();
    if let Ok(p) = std::env::var("PDFIUM_LIB") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
    }
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join(&name));
            cands.push(dir.join("lib").join(&name));
            // macOS .app: Contents/MacOS/exe -> Contents/Frameworks/
            cands.push(dir.join("../Frameworks").join(&name));
        }
    }
    cands.push(PathBuf::from("vendor").join(&name));
    cands.push(PathBuf::from("target/vendor").join(&name));
    for c in cands {
        if c.is_file() {
            return Ok(c);
        }
    }
    Err(anyhow!(
        "找不到 {shown}。放到程序同目录, 或用环境变量 PDFIUM_LIB 指定路径。"
    ))
}
