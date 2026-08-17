//! 扫描件 PDF -> 可编辑 Word / Excel, 全本地运行
//!
//! 流水线: pdfium 渲染 -> PP-OCRv6 识别 -> 规则版面重建 -> 手写 OOXML / xlsx。
//! 与 Python 版(pdfRec/convert.py)判据完全一致, 阈值逐条照搬。
//!
//! 逐页流式识别: 渲染一页 -> 识别 -> 重建 -> 页图随即释放。Python 版靠把每页
//! PNG 落盘来控内存, 这里不落盘也不占内存。
//!
//! 排版是识别全做完之后再走一趟。缩进的零点要看全文最靠左的那一档, 边识别边排
//! 就只能拿前几页的数据当全文用(详见 render::scan_indents)。留在内存里的只有
//! 各页的文字框, 峰值仍然由识别那一段决定。

pub mod config;
pub mod docx;
pub mod geom;
pub mod i18n;
pub mod imgutil;
pub mod layout;
pub mod ocr;
pub mod ocrlang;
pub mod pdf;
pub mod render;
pub mod xlsx;

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

use config::{Config, Format};
use i18n::K;
use layout::Page;
use ocr::Item;

/// 进度阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Render,
    Ocr,
    Layout,
    Done,
}

/// 单页失败的记录 —— 收集起来而不是中断整篇
#[derive(Debug, Clone)]
pub struct PageError {
    pub page: usize,
    pub stage: &'static str,
    pub msg: String,
}

/// 进度回调: (阶段, 已完成, 总数, 当前在做什么)
pub type ProgressFn<'a> = &'a dyn Fn(Stage, usize, usize, &str);

/// 外部挂钩: 进度、日志、中断
///
/// 不要求 Send/Sync —— 挂钩只在 convert 这一次调用里用, 调用方自己决定放哪个
/// 线程。加上界限反而让 GUI 那边没法直接塞一个捕获了 Sender 的闭包。
#[derive(Default, Clone, Copy)]
pub struct Hooks<'a> {
    pub progress: Option<ProgressFn<'a>>,
    pub log: Option<&'a dyn Fn(&str)>,
    /// 返回 true 时中断
    pub stop: Option<&'a dyn Fn() -> bool>,
}

impl Hooks<'_> {
    fn tick(&self, stage: Stage, cur: usize, total: usize, msg: &str) {
        if let Some(f) = self.progress {
            f(stage, cur, total, msg);
        }
    }
    fn say(&self, msg: &str) {
        match self.log {
            Some(f) => f(msg),
            None => println!("{msg}"),
        }
    }
    fn check(&self) -> Result<()> {
        match self.stop {
            Some(f) if f() => Err(anyhow!("{}", tr!(K::LibCancelled))),
            _ => Ok(()),
        }
    }
}

pub struct Outcome {
    pub outputs: Vec<PathBuf>,
    pub errors: Vec<PageError>,
    pub pages: usize,
}

/// 一次转换需要的两样重家伙: pdfium 绑定 + 三个 ONNX 会话
///
/// 建一次能转很多份 —— 模型加载要一两秒, 批量转换时不该每份都付一遍。
pub struct Converter {
    renderer: pdf::Renderer,
    engine: ocr::Engine,
}

impl Converter {
    pub fn new(model_dir: &Path) -> Result<Self> {
        Self::new_with(model_dir, ocr::EngineOptions::default())
    }

    /// 指定识别语言包等开关
    pub fn new_with(model_dir: &Path, opts: ocr::EngineOptions) -> Result<Self> {
        Ok(Self {
            renderer: pdf::Renderer::new()?,
            engine: ocr::Engine::load_with(model_dir, opts)?,
        })
    }

    /// 用默认位置的模型
    pub fn with_default_models() -> Result<Self> {
        Self::new(&locate_models()?)
    }

    /// 换识别语言: 只重建 OCR 会话, 渲染器一动不动
    ///
    /// 不能靠"整个 Converter 重建一遍"来换语言 —— pdfium 的绑定在
    /// pdfium-render 里是进程级全局的, 再初始化一次会直接报错。
    pub fn reload_ocr(&mut self, model_dir: &Path, opts: ocr::EngineOptions) -> Result<()> {
        self.engine = ocr::Engine::load_with(model_dir, opts)?;
        Ok(())
    }

    /// 转换一份 PDF, 返回输出路径
    pub fn convert(
        &mut self,
        pdf: &Path,
        out_dir: Option<&Path>,
        cfg: &Config,
        fmt: Format,
        hooks: &Hooks,
    ) -> Result<Outcome> {
        if !pdf.is_file() {
            return Err(anyhow!("{}", tr!(K::LibNoFile, pdf.display())));
        }
        let pdf = pdf.canonicalize().unwrap_or_else(|_| pdf.to_path_buf());
        let name = pdf
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "DOC".into());
        let dir = out_dir
            .map(|d| d.to_path_buf())
            .or_else(|| pdf.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        std::fs::create_dir_all(&dir)?;
        let cache = Cache::new(pdf.parent().unwrap_or(Path::new(".")), &name, cfg.long_edge);

        // renderer 与 engine 是两个字段, 拆开借才不会撞 —— 渲染要 &self,
        // OCR 会话要 &mut self
        let Self { renderer, engine } = self;
        let pages = renderer.open(&pdf, cfg.long_edge)?;
        let total = pages.len();
        if total == 0 {
            return Err(anyhow!("{}", tr!(K::LibNoPages)));
        }
        hooks.say(&tr!(K::LibStart, name, total));

        let landscape = pages.mostly_landscape();
        let mut doc = fmt.wants_docx().then(|| {
            let mut d = docx::Docx::new(cfg, landscape);
            d.para(
                &name,
                &docx::Fmt::new(16.0).bold(true),
                0,
                docx::Align::Center,
                false,
            );
            d
        });
        let mut book = fmt.wants_xlsx().then(|| xlsx::Book::new(&name));
        let mut st = render::State::default();
        let mut errors: Vec<PageError> = Vec::new();

        // 第一趟: 识别 + 攒缩进档位。排版留到第二趟 —— 缩进的零点是全文最靠
        // 左的那一档, 边识别边排的话前几页手上还没有后面的数据, 同一个横坐标
        // 在前后两页会落到不同的缩进级(详见 render::scan_indents)。
        //
        // 代价只是把各页的文字框留在内存里。框里存的是坐标加文字, 96 页的合同
        // 也就几 MB, 峰值仍然是 OCR 那一段说了算, 排版这边看不见。
        let mut laid: Vec<Result<Page, String>> = Vec::with_capacity(total);
        for i in 0..total {
            hooks.check()?;
            let no = i + 1;
            hooks.tick(Stage::Ocr, no, total, &tr!(K::LibStageOcr, no, total));
            match one_page(&pages, engine, &cache, i, cfg, hooks) {
                Ok(page) => {
                    render::scan_indents(&mut st, &page, cfg);
                    // xlsx 不吃缩进, 顺手就写了
                    if let Some(b) = book.as_mut() {
                        b.add_page(&page, no);
                    }
                    laid.push(Ok(page));
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    hooks.say(&tr!(K::LibPageFailed, no, msg));
                    errors.push(PageError {
                        page: no,
                        stage: i18n::t(K::LibStageLayout),
                        msg: msg.clone(),
                    });
                    if let Some(b) = book.as_mut() {
                        b.page_failed(no, &msg);
                    }
                    laid.push(Err(msg));
                }
            }
        }

        // 第二趟: 零点定下来了, 再排版
        if let Some(d) = doc.as_mut() {
            for (i, r) in laid.iter().enumerate() {
                hooks.check()?;
                let no = i + 1;
                match r {
                    Ok(page) => {
                        if cfg.page_marker {
                            render::page_marker(d, page, no, &mut st);
                        }
                        render::render_page(d, page, no, cfg, &mut st);
                    }
                    Err(msg) => render::page_failed(d, no, msg, &mut st),
                }
            }
        }

        hooks.check()?;
        hooks.tick(Stage::Layout, total, total, i18n::t(K::LibStageWrite));
        let mut outputs = Vec::new();
        if let Some(d) = doc {
            let out = vacant(&dir, &name, "docx", hooks);
            d.save(&out)?;
            hooks.say(&tr!(K::LibOutDocx, out.display(), total, kb(&out)));
            outputs.push(out);
        }
        if let Some(b) = book {
            let out = vacant(&dir, &name, "xlsx", hooks);
            let n = b.save(&out)?;
            hooks.say(&tr!(K::LibOutXlsx, out.display(), n, kb(&out)));
            outputs.push(out);
        }
        if !errors.is_empty() {
            hooks.say(&tr!(K::LibFileErrors, errors.len()));
        }
        hooks.tick(
            Stage::Done,
            total,
            total,
            &outputs
                .last()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        Ok(Outcome {
            outputs,
            errors,
            pages: total,
        })
    }
}

/// 渲染 + 识别 + 版面重建一页
///
/// 独立成函数而不是 Converter 的方法: convert 里已经把 renderer / engine 两个
/// 字段拆开借了, 再走 &self 会把整个 Converter 重新借一遍, 借用检查过不了。
fn one_page(
    pages: &pdf::Pages,
    engine: &mut ocr::Engine,
    cache: &Cache,
    i: usize,
    cfg: &Config,
    hooks: &Hooks,
) -> Result<layout::Page> {
    let img = pages
        .render(i)
        .with_context(|| tr!(K::LibRenderPage, i + 1))?;
    let items = match cache.load(i) {
        Some(v) => v,
        None => {
            let v = engine
                .run(&img)
                .with_context(|| tr!(K::LibOcrPageCtx, i + 1))?;
            cache.save(i, &v);
            v
        }
    };
    let page = layout::analyze(&items, &img, cfg);
    let tbls = page
        .blocks
        .iter()
        .filter(|b| matches!(b, layout::Block::Table(..)))
        .count();
    let grids = page
        .blocks
        .iter()
        .filter(|b| matches!(b, layout::Block::Grid(_)))
        .count();
    hooks.say(&i18n::lib_page(
        i + 1,
        items.len(),
        page.blocks.len(),
        tbls,
        grids,
        page.header.len() + page.footer.len(),
    ));
    Ok(page)
}

fn kb(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len() / 1024).unwrap_or(0)
}

/// 挑一个没被占用的输出路径: 同名文件已存在就写成 `xxx-20260813224500.docx`
///
/// **一律不覆盖。** 转出来的 Word 通常是要人接着改的 —— 补签章、调格式、改错字。
/// 换个参数重跑一次就把一下午的修订盖没了, 而且没有回收站可捞(程序是直接
/// 覆写文件)。宁可多留一份让人自己删。
///
/// 时间戳仍撞上的话(docx/xlsx 同秒写出、或者一秒内连点两次)再挂序号。
/// 用 symlink_metadata 而不是 exists(): 断掉的符号链接 exists() 报 false,
/// 但那个位置确实被占着。
fn vacant(dir: &Path, stem: &str, ext: &str, hooks: &Hooks) -> PathBuf {
    let taken = |p: &Path| p.symlink_metadata().is_ok();
    let first = dir.join(format!("{stem}.{ext}"));
    if !taken(&first) {
        return first;
    }
    let ts = chrono::Local::now().format("%Y%m%d%H%M%S");
    let mut out = dir.join(format!("{stem}-{ts}.{ext}"));
    let mut n = 2;
    while taken(&out) {
        out = dir.join(format!("{stem}-{ts}-{n}.{ext}"));
        n += 1;
    }
    hooks.say(&tr!(
        K::LibExists,
        first.display(),
        out.file_name().unwrap_or_default().to_string_lossy()
    ));
    out
}

/// OCR 结果缓存
///
/// 识别占了九成时间, 渲染反而很快 —— 所以只缓存识别结果, 页图每次重渲。
/// 缓存目录跟 PDF 放一起, 便于手工清理。
struct Cache {
    dir: Option<PathBuf>,
    tag: String,
    long_edge: u32,
}

impl Cache {
    fn new(base: &Path, name: &str, long_edge: u32) -> Self {
        let tag: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(c))
            .take(16)
            .collect();
        let tag = if tag.is_empty() { "DOC".into() } else { tag };
        let dir = base.join(".pdf2doc_cache");
        let dir = std::fs::create_dir_all(&dir).ok().map(|_| dir);
        Self {
            dir,
            tag,
            long_edge,
        }
    }

    fn path(&self, i: usize) -> Option<PathBuf> {
        self.dir
            .as_ref()
            .map(|d| d.join(format!("{}_{}_{:04}.json", self.tag, self.long_edge, i + 1)))
    }

    fn load(&self, i: usize) -> Option<Vec<Item>> {
        let p = self.path(i)?;
        let s = std::fs::read_to_string(p).ok()?;
        serde_json::from_str(&s).ok()
    }

    fn save(&self, i: usize, items: &[Item]) {
        if let (Some(p), Ok(s)) = (self.path(i), serde_json::to_string(items)) {
            let _ = std::fs::write(p, s);
        }
    }
}

/// 用户缓存目录
///
/// 只为找模型用一次, 不值得为它引 dirs —— 那条依赖链上挂着 MPL-2.0 的 option-ext,
/// 会给闭源分发添一条要交代的授权。
fn cache_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(target_os = "windows") {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library/Caches"))
    } else {
        var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))
    }
}

/// 用户配置目录
///
/// 跟 [`cache_dir`] 分开: 界面语言这种东西是用户明确设过的, 不该在他清一次
/// 缓存之后自己变回去; 语言包相反, 那些是能重新下回来的, 放缓存里正合适。
pub fn config_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(target_os = "windows") {
        var("APPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library/Application Support"))
    } else {
        var("XDG_CONFIG_HOME").or_else(|| var("HOME").map(|h| h.join(".config")))
    }
}

/// 找模型目录: 环境变量 -> 可执行文件同目录 -> 用户缓存目录 -> 源码树
pub fn locate_models() -> Result<PathBuf> {
    let ok = |p: &Path| p.join("PP-OCRv6_rec_small.onnx").is_file();
    if let Ok(p) = std::env::var("PDF2DOC_MODELS") {
        let p = PathBuf::from(p);
        if ok(&p) {
            return Ok(p);
        }
    }
    let mut cands = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            cands.push(d.join("models"));
            // macOS .app: Contents/MacOS/exe -> Contents/Resources/models
            cands.push(d.join("../Resources/models"));
        }
    }
    if let Some(d) = cache_dir() {
        cands.push(d.join("scannedpdf2doc/models"));
    }
    cands.push(PathBuf::from("models"));
    cands.push(PathBuf::from("vendor/models"));
    for c in cands {
        if ok(&c) {
            return Ok(c);
        }
    }
    Err(anyhow!("{}", tr!(K::LibNoModels)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 不覆盖是这个程序对用户的承诺, 用例盯死它
    #[test]
    fn vacant_never_returns_an_occupied_path() {
        let dir = std::env::temp_dir().join(format!("pdf2doc-vacant-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let h = Hooks::default();

        // 空目录: 就用本名
        let p1 = vacant(&dir, "a", "docx", &h);
        assert_eq!(p1, dir.join("a.docx"));

        // 本名被占: 改成带时间戳的, 且原文件分毫不动
        std::fs::write(&p1, "人工改过的内容").expect("写占位文件");
        let p2 = vacant(&dir, "a", "docx", &h);
        assert_ne!(p2, p1);
        assert!(!p2.exists(), "挑出来的名字必须是空的");
        let stem = p2.file_stem().unwrap().to_string_lossy().to_string();
        assert!(
            stem.starts_with("a-") && stem.len() >= 16,
            "时间戳格式: {stem}"
        );
        assert_eq!(p2.extension().unwrap(), "docx");

        // 时间戳那个也被占: 再让一次(同一秒挂序号, 跨秒换时间戳, 都行)
        std::fs::write(&p2, b"x").expect("写占位文件");
        let p3 = vacant(&dir, "a", "docx", &h);
        assert!(!p3.exists() && p3 != p1 && p3 != p2);

        assert_eq!(std::fs::read(&p1).unwrap(), "人工改过的内容".as_bytes());
        std::fs::remove_dir_all(&dir).ok();
    }
}
