//! PDF 渲染 —— pdfium
//!
//! Python 版用 PyMuPDF(整包 46 MB, 光 libmupdf.dylib 就 32 MB)。pdfium 的
//! 动态库约 7 MB, 渲染质量与速度都够, 是这次换栈能省下来的第二大头。
//!
//! 逐页渲染而不是一次全渲染: 300 dpi 的 A4 灰度图一页 8.7 MB, 一份 96 页的
//! 合同全存内存就是 800 MB。Python 版靠落盘缓存绕开, 这里直接流式处理。

use anyhow::{anyhow, Context, Result};
use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};

use crate::imgutil::Gray;

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

    /// 渲染一页成灰度图
    ///
    /// 只要灰度: 后面无论 OCR 还是框线检测都在灰度上做, 早一步转掉能省 2/3 内存。
    /// dpi 按本页尺寸倒推, 让长边落在 long_edge 附近 —— 页面大小不一的卷宗里,
    /// 固定 dpi 会让 A3 图纸渲成八千像素、A5 单据只有一千, 阈值没法通用。
    pub fn render(&self, i: usize) -> Result<Gray> {
        let page = self.doc.pages().get(i as PdfPageIndex)?;
        let long_pt = page.width().value.max(page.height().value).max(1.0);
        let dpi = (self.long_edge as f32 / long_pt * 72.0)
            .round()
            .clamp(150.0, 300.0);
        let cfg = PdfRenderConfig::new().scale_page_by_factor(dpi / 72.0);
        let img = page.render_with_config(&cfg)?.as_image()?.into_luma8();
        Ok(Gray {
            w: img.width() as usize,
            h: img.height() as usize,
            px: img.into_raw(),
        })
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
