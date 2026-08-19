//! 图形界面: 一个批量转换队列 + 一块参数面板
//!
//! 转换跑在工作线程上, 通过 channel 把进度和日志送回来 —— 主线程只管画。
//! Converter 在工作线程内部创建, 从不跨线程传递, 省得跟 ort / pdfium 的
//! Send 界限较劲。
//!
//! 工作线程是**常驻**的, 一批批任务从 channel 里取: Converter 因此只建一次。
//! 每批都新建的话, 32 MB 模型要重载一遍(一两秒), pdfium 的进程级绑定也会被
//! 重复初始化。
//!
//! 界面语言和识别语言是两件事, 各有各的选单: 前者决定按钮上写什么字, 后者
//! 决定纸上的字认不认得出来。一个韩国用户完全可能把界面设成韩语却整天扫中文
//! 合同 —— 反过来也一样。

#![cfg_attr(windows, windows_subsystem = "windows")]

use eframe::egui;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use scannedpdf2doc::config::{Config, Format};
use scannedpdf2doc::i18n::{self, Lang, K};
use scannedpdf2doc::ocr::EngineOptions;
use scannedpdf2doc::ocrlang::{self, Pack};
use scannedpdf2doc::{locate_models, tr, Converter, Hooks, Stage};

#[derive(Clone, PartialEq)]
enum Status {
    Waiting,
    Running,
    /// 存的是真实路径而不是一段文字: 点队列里那个 ✓ 要能直接把文件打开
    Done {
        outs: Vec<PathBuf>,
        warn: Option<String>,
    },
    Failed(String),
}

#[derive(Clone)]
struct Job {
    path: PathBuf,
    /// 单独指定的输出格式; None = 跟顶栏的默认走
    ///
    /// 存成 Option 而不是加进来时把默认拷一份: 拷一份的话, 之后再改顶栏就只对
    /// 新加的文件生效, 一队文件想统一改格式还得一个个点。
    fmt: Option<Format>,
    status: Status,
}

enum Msg {
    Log(String),
    Progress {
        job: usize,
        cur: usize,
        total: usize,
        text: String,
    },
    Job {
        job: usize,
        status: Status,
    },
    AllDone,
}

/// 底部进度条要显示的东西
#[derive(Default)]
struct Prog {
    /// 当前是队列里第几个(0 基)
    job: usize,
    cur: usize,
    total: usize,
    text: String,
}

/// 识别用哪个 rec 模型: (放在哪个目录, 文件名)。None = 内置那个中英混排的
type Rec = Option<(PathBuf, String)>;

/// 一批任务: 主线程攒好丢给常驻工作线程
struct Batch {
    /// (在队列里的下标, 文件, 输出格式)
    ///
    /// 下标要一起带着, 不能靠这个 Vec 自己的位置 —— 「只重转失败的那一个」
    /// 送过来的就是一条任务, 而它在队列里排第七, 进度得报回第七行。
    tasks: Vec<(usize, PathBuf, Format)>,
    cfg: Config,
    out: Option<PathBuf>,
    /// 这一批按哪种语言认。跟上一批不一样的话工作线程会换掉识别模型
    rec: Rec,
    tx: Sender<Msg>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
}

/// 正在下的那个语言包
struct Dl {
    pack: &'static Pack,
    /// 0~100, 下载线程写, 主线程读
    pct: Arc<AtomicU32>,
    /// 下完(或失败)时来一条
    rx: Receiver<Result<(), String>>,
}

/// 记在盘上的那两项选择
///
/// 只存这两个。转换参数不存 —— 那些是"这一批想怎么转", 每次都可能不一样;
/// 语言是"我是谁", 设一次就该一直算数。
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Settings {
    /// 界面语言的 tag; 没有 = 跟系统走
    lang: Option<String>,
    /// 识别语言的 code; 没有 = 内置
    ocr: Option<String>,
}

impl Settings {
    fn path() -> Option<PathBuf> {
        let d = scannedpdf2doc::config_dir()?.join("scannedpdf2doc");
        std::fs::create_dir_all(&d).ok()?;
        Some(d.join("settings.json"))
    }

    fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// 存不下去就算了 —— 为一个选单的记忆弹一个报错框不值当
    fn save(&self) {
        if let (Some(p), Ok(s)) = (Self::path(), serde_json::to_string_pretty(self)) {
            let _ = std::fs::write(p, s);
        }
    }
}

/// 界面上会用到的几个颜色
///
/// 不用 egui 自带的那几档, 也不用原来那组值: 它们**在浅色主题下都不到
/// 4.5:1** —— `weak` 的灰 3.4、原来那个绿 3.6、蓝 4.2、灰 4.0。队列里那些
/// 状态本来就是小字, 差这一点就是"看得见"和"看得清"的区别。
///
/// 深浅两套分开算: 一套颜色不可能在白底和近黑底上都够对比度 —— 在白底上够深
/// 的绿, 放到深色主题里就成了一团发黑的东西。
///
/// 每个值都对着各自主题的背景算过, 最低的一档也有 5.3:1。
struct Pal {
    /// 等待中
    idle: egui::Color32,
    /// 正在转
    busy: egui::Color32,
    /// 成了
    ok: egui::Color32,
    /// 砸了
    bad: egui::Color32,
    /// 次要说明文字 —— 比正文浅, 但仍然读得清
    dim: egui::Color32,
    /// 主操作按钮的底色与其上的字
    cta: (egui::Color32, egui::Color32),
    /// 停止按钮同上
    halt: (egui::Color32, egui::Color32),
}

fn pal(ui: &egui::Ui) -> Pal {
    let c = egui::Color32::from_rgb;
    if ui.visuals().dark_mode {
        Pal {
            idle: c(0xA0, 0xA0, 0xA0),
            busy: c(0x7A, 0xB0, 0xFF),
            ok: c(0x6B, 0xD4, 0x8A),
            bad: c(0xFF, 0x8A, 0x80),
            dim: c(0xB0, 0xB0, 0xB0),
            cta: (c(0x14, 0xB8, 0xA6), egui::Color32::BLACK),
            halt: (c(0xF9, 0x70, 0x66), egui::Color32::BLACK),
        }
    } else {
        Pal {
            idle: c(0x6B, 0x6B, 0x6B),
            busy: c(0x1A, 0x5F, 0xBF),
            ok: c(0x1E, 0x7A, 0x38),
            bad: c(0xB4, 0x23, 0x18),
            dim: c(0x5C, 0x5C, 0x5C),
            cta: (c(0x0F, 0x76, 0x6E), egui::Color32::WHITE),
            halt: (c(0xB4, 0x23, 0x18), egui::Color32::WHITE),
        }
    }
}

/// 一个填色的主操作按钮
///
/// 整个界面就这么一处填色。顶栏上七八个按钮长得一模一样时, 「开始转换」跟
/// 「清空」看着一样重 —— 而它们一个是这屏的目的, 一个是随手一按就毁掉队列的。
fn solid(text: &str, (fill, fg): (egui::Color32, egui::Color32)) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text).color(fg).strong()).fill(fill)
}

struct App {
    jobs: Vec<Job>,
    cfg: Config,
    fmt: Format,
    out_dir: Option<PathBuf>,
    log: Vec<String>,
    rx: Option<Receiver<Msg>>,
    /// 常驻工作线程的入口, 首次开始转换时才建 —— 没转过就不该付模型加载的钱
    job_tx: Option<Sender<Batch>>,
    stop: Arc<AtomicBool>,
    running: bool,
    /// 点过停止但工作线程还没走完当前这一页 —— 按钮要变成"正在停止…"给个交代,
    /// 不然人会以为没点上, 连点几次
    stopping: bool,
    cur: Prog,
    /// 本批开始的时刻, 用来算已用时和粗略的剩余时间
    started: Option<std::time::Instant>,
    /// 上一批的结论, 停在底部状态条上 —— 转完就消失的话, 人转身倒杯水回来就
    /// 不知道到底成了没有
    summary: Option<String>,
    show_advanced: bool,
    /// 刚被移出队列的那几项, 留着给「撤销」
    ///
    /// 「清空」一按就没了, 而队列可能是从三个文件夹里一个个挑出来的。与其
    /// 弹个「确定要清空吗」拦在每一次(包括那 99 次真想清空的), 不如让它可以
    /// 收回来 —— 按错的代价降到一次点击, 按对的代价是零。
    undo: Vec<Job>,
    /// 这一批送出去的是队列里的哪几行 —— 进度按它算, 不按整个队列算
    batch: Vec<usize>,

    // ── 语言 ──
    lang: Lang,
    /// 选中的识别语言
    ocr: &'static Pack,
    /// 已经装好的语言包 code。缓存起来而不是每帧去 stat 一遍磁盘
    have: HashSet<&'static str>,
    dl: Option<Dl>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, st: Settings) -> Self {
        install_ui_font(&cc.egui_ctx, i18n::cur());
        // 上次选的包这次未必还在(用户清了缓存, 或者手工删了文件)。这种时候
        // 退回内置, 而不是让选单上写着「韩语」却拿中文模型去认 —— 那出来的
        // 不是"没结果", 是一整页看着像结果的错字
        let want = ocrlang::by_code(st.ocr.as_deref().unwrap_or(""));
        let ocr = if want.installed() {
            want
        } else {
            &ocrlang::PACKS[0]
        };
        Self {
            jobs: Vec::new(),
            cfg: Config::default(),
            fmt: Format::Docx,
            out_dir: None,
            log: Vec::new(),
            rx: None,
            job_tx: None,
            stop: Arc::new(AtomicBool::new(false)),
            running: false,
            stopping: false,
            cur: Prog::default(),
            started: None,
            summary: None,
            show_advanced: false,
            undo: Vec::new(),
            batch: Vec::new(),
            lang: i18n::cur(),
            ocr,
            have: ocrlang::PACKS
                .iter()
                .filter(|p| p.installed())
                .map(|p| p.code)
                .collect(),
            dl: None,
        }
    }

    fn save(&self) {
        Settings {
            lang: Some(self.lang.tag().into()),
            ocr: Some(self.ocr.code.into()),
        }
        .save();
    }

    /// 这一批该用哪个 rec 模型
    ///
    /// 选中的包不在本地就退回内置 —— 正常走不到这条: 选包那一步就已经把它下
    /// 下来了(下不成的话选择根本不会生效), 启动时也核过一遍。留着是防着
    /// "程序开着的时候文件被删了"。
    fn rec(&self) -> Rec {
        if self.ocr.builtin() || !self.have.contains(self.ocr.code) {
            return None;
        }
        ocrlang::installed_dir(self.ocr).map(|d| (d, self.ocr.file.to_string()))
    }

    /// 加文件, 顺带说清楚加了几个、跳了几个
    ///
    /// 拖进来的可能是文件夹 —— 一沓扫描件本来就是按文件夹存的, 让人先进目录
    /// 全选再拖是白费一道手。默认行为原来是"不是 .pdf 就默默忽略", 拖一个
    /// 文件夹进来毫无反应, 看着像程序坏了。
    fn add(&mut self, paths: Vec<PathBuf>) {
        let (mut added, mut dup, mut skipped) = (0usize, 0usize, 0usize);
        let mut pdfs = Vec::new();
        for p in paths {
            if p.is_dir() {
                let before = pdfs.len();
                collect_pdfs(&p, 0, &mut pdfs);
                if pdfs.len() == before {
                    self.log.push(tr!(K::NoPdfIn, p.display()));
                }
            } else if is_pdf(&p) {
                pdfs.push(p);
            } else {
                skipped += 1;
            }
        }
        pdfs.sort();
        for p in pdfs {
            if self.jobs.iter().any(|j| j.path == p) {
                dup += 1;
                continue;
            }
            self.jobs.push(Job {
                path: p,
                fmt: None,
                status: Status::Waiting,
            });
            added += 1;
        }
        let mut note = tr!(K::Added, added);
        if dup > 0 {
            note += &tr!(K::AddedDup, dup);
        }
        if skipped > 0 {
            note += &tr!(K::AddedSkip, skipped);
        }
        if self.running && added > 0 {
            // 这一批的任务清单在点开始的那一刻就定死了, 现在加的赶不上
            note += i18n::t(K::AddedRunning);
        }
        if added + dup + skipped > 0 {
            self.log.push(note);
        }
    }

    /// 移出队列, 留一份给「撤销」
    fn remove(&mut self, which: &[usize]) {
        self.undo = which
            .iter()
            .filter_map(|&i| self.jobs.get(i))
            .cloned()
            .collect();
        // 从后往前删, 不然删掉前一个之后后面的下标全变了
        for &i in which.iter().rev() {
            if i < self.jobs.len() {
                self.jobs.remove(i);
            }
        }
        if !self.undo.is_empty() {
            self.log.push(tr!(K::Removed, self.undo.len()));
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let all: Vec<usize> = (0..self.jobs.len()).collect();
        self.start_some(ctx, &all, true);
    }

    /// 只转其中几个
    ///
    /// `reset` = 把这几个之外的也退回「等待」。整队重来时要, 「重转失败那一个」
    /// 时不要 —— 那会把旁边已经转好的绿勾抹成灰圈, 看着像白干了一场。
    fn start_some(&mut self, ctx: &egui::Context, which: &[usize], reset: bool) {
        if self.running || which.is_empty() || self.dl.is_some() {
            return;
        }
        for (i, j) in self.jobs.iter_mut().enumerate() {
            if reset || which.contains(&i) {
                j.status = Status::Waiting;
            }
        }
        if reset {
            self.log.clear();
        }
        self.batch = which.to_vec();
        self.running = true;
        self.stopping = false;
        self.summary = None;
        self.started = Some(std::time::Instant::now());
        self.stop = Arc::new(AtomicBool::new(false));

        let (tx, rx) = channel();
        self.rx = Some(rx);
        let batch = Batch {
            tasks: which
                .iter()
                .filter_map(|&i| {
                    let j = self.jobs.get(i)?;
                    Some((i, j.path.clone(), j.fmt.unwrap_or(self.fmt)))
                })
                .collect(),
            cfg: self.cfg.clone(),
            out: self.out_dir.clone(),
            rec: self.rec(),
            tx,
            stop: self.stop.clone(),
            ctx: ctx.clone(),
        };
        let job_tx = self.job_tx.get_or_insert_with(|| {
            let (jtx, jrx) = channel::<Batch>();
            std::thread::spawn(move || worker(jrx));
            jtx
        });
        // 工作线程只会在主线程退出(job_tx 被丢掉)时结束, 正常跑不到这条
        if job_tx.send(batch).is_err() {
            self.log.push(i18n::t(K::WorkerDead).into());
            self.running = false;
            self.rx = None;
        }
    }

    /// 整批的完成比例, 外加一行人话
    ///
    /// 各文件页数事先不知道, 所以按"已完成的文件数 + 当前文件内的比例"折算 ——
    /// 页数悬殊时不准, 所以剩余时间写成"约"。宁可给个粗数, 也好过只报页码让人
    /// 自己心算还要等多久。
    fn overall(&self) -> (f32, String) {
        let n = self.batch.len().max(1);
        let (ok, bad) = self.done_failed();
        let done = ok + bad;
        let inner = if self.cur.total > 0 {
            self.cur.cur as f32 / self.cur.total as f32
        } else {
            0.0
        };
        let frac = ((done as f32 + inner) / n as f32).clamp(0.0, 1.0);

        let name = self
            .jobs
            .get(self.cur.job)
            .and_then(|j| j.path.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        // 报的是"这一批里的第几个", 不是"队列里的第几行" —— 只重转一个失败的
        // 时候, 那行排第七也该显示 (1/1)
        let at = self
            .batch
            .iter()
            .position(|&i| i == self.cur.job)
            .map_or(0, |k| k + 1);
        let mut s = format!("({}/{}) {name}", at.max(1), n);
        if !self.cur.text.is_empty() {
            s += &format!(" · {}", self.cur.text);
        }
        if let Some(t) = self.started {
            let el = t.elapsed().as_secs();
            s += &tr!(K::Elapsed, dur(el));
            // 头 5% 的外推纯属瞎猜, 别显示
            if frac > 0.05 {
                s += &tr!(K::Eta, dur((el as f32 * (1.0 - frac) / frac) as u64));
            }
        }
        (frac, s)
    }

    /// 这一批里成了几个、砸了几个
    fn done_failed(&self) -> (usize, usize) {
        let (mut ok, mut bad) = (0, 0);
        for j in self.batch.iter().filter_map(|&i| self.jobs.get(i)) {
            match j.status {
                Status::Done { .. } => ok += 1,
                Status::Failed(_) => bad += 1,
                _ => {}
            }
        }
        (ok, bad)
    }

    /// 「打开结果」按钮该打开哪儿: 指定过输出目录就是它, 否则是最近一份产出所在的目录
    fn result_dir(&self) -> Option<PathBuf> {
        if self.running {
            return None;
        }
        if let Some(d) = &self.out_dir {
            return self
                .jobs
                .iter()
                .any(|j| matches!(j.status, Status::Done { .. }))
                .then(|| d.clone());
        }
        self.jobs
            .iter()
            .rev()
            .find_map(|j| match &j.status {
                Status::Done { outs, .. } => outs.first().and_then(|p| p.parent()),
                _ => None,
            })
            .map(|p| p.to_path_buf())
    }

    fn drain(&mut self) {
        let Some(rx) = &self.rx else { return };
        for m in rx.try_iter().collect::<Vec<_>>() {
            match m {
                Msg::Log(s) => {
                    self.log.push(s);
                    if self.log.len() > 2000 {
                        self.log.drain(..500);
                    }
                }
                Msg::Progress {
                    job,
                    cur,
                    total,
                    text,
                } => {
                    self.cur = Prog {
                        job,
                        cur,
                        total,
                        text,
                    };
                    if let Some(j) = self.jobs.get_mut(job) {
                        j.status = Status::Running;
                    }
                }
                Msg::Job { job, status } => {
                    if let Some(j) = self.jobs.get_mut(job) {
                        j.status = status;
                    }
                }
                Msg::AllDone => {
                    let (ok, bad) = self.done_failed();
                    let left = self.batch.len().saturating_sub(ok + bad);
                    let mut s = tr!(K::SumDone, ok);
                    if bad > 0 {
                        s += &tr!(K::SumFailed, bad);
                    }
                    if left > 0 {
                        s += &tr!(K::SumLeft, left);
                    }
                    if let Some(t) = self.started {
                        s += &tr!(K::SumTime, dur(t.elapsed().as_secs()));
                    }
                    self.log.push(s.clone());
                    self.summary = Some(s);
                    self.running = false;
                    self.stopping = false;
                    self.rx = None;
                    self.cur = Prog::default();
                    break;
                }
            }
        }
    }

    // ───────────────────────── 语言 ─────────────────────────

    /// 换界面语言: 连字体一起换
    ///
    /// 已经打出来的日志不会跟着变。它们是一份流水账, 记的是当时说了什么;
    /// 回头把历史记录改成另一种语言, 反而对不上人当时看到的东西。
    fn set_lang(&mut self, ctx: &egui::Context, l: Lang) {
        if l == self.lang {
            return;
        }
        self.lang = l;
        i18n::set(l);
        install_ui_font(ctx, l);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(i18n::t(K::WindowTitle).into()));
        self.save();
    }

    /// 选识别语言: 包不在就先下, 下成了才真的切过去
    ///
    /// 不下成就不切, 是这里最要紧的一条。切过去而包是空的, 下一次转换会拿中文
    /// 模型去认韩文, 出来的不是"没结果", 是一整页**看着像结果的错字**。
    fn pick_ocr(&mut self, ctx: &egui::Context, p: &'static Pack) {
        if p.builtin() || self.have.contains(p.code) {
            self.ocr = p;
            self.save();
            return;
        }
        if self.dl.is_some() || self.running {
            return;
        }
        let pct = Arc::new(AtomicU32::new(0));
        let (tx, rx) = channel();
        let (bar, c) = (pct.clone(), ctx.clone());
        std::thread::spawn(move || {
            let r = ocrlang::download(p, &|v| {
                bar.store(v, Ordering::Relaxed);
                c.request_repaint();
            });
            let _ = tx.send(r.map(|_| ()).map_err(|e| format!("{e:#}")));
            c.request_repaint();
        });
        self.dl = Some(Dl { pack: p, pct, rx });
    }

    /// 下载线程有结果了就收下
    fn poll_download(&mut self) {
        let done = match self.dl.as_ref() {
            Some(d) => d.rx.try_recv().ok().map(|r| (d.pack, r)),
            None => None,
        };
        let Some((pack, r)) = done else { return };
        self.dl = None;
        match r {
            Ok(()) => {
                self.have.insert(pack.code);
                self.ocr = pack;
                self.save();
                self.log.push(tr!(K::PackReady, i18n::t(pack.name)));
            }
            Err(e) => self.log.push(tr!(K::PackFailed, i18n::t(pack.name), e)),
        }
    }

    /// 「识别语言 [中文·英文 ▾]」那一格
    fn ocr_picker(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // 先把要画的东西取出来: 下面的闭包要用 &mut self 记选择, 同时读
        // self.have 会撞车
        let rows: Vec<(&'static Pack, bool)> = ocrlang::PACKS
            .iter()
            .map(|p| (p, p.builtin() || self.have.contains(p.code)))
            .collect();
        let cur = self.ocr;
        let mut chosen: Option<&'static Pack> = None;

        egui::ComboBox::from_id_salt("ocr_lang")
            .width(200.0)
            .selected_text(i18n::t(cur.name))
            .show_ui(ui, |ui| {
                for (p, have) in &rows {
                    // 没下的那几个后面挂上体积 —— 点下去要花的流量, 该在点之前
                    // 就看得见
                    let label = if *have {
                        i18n::t(p.name).to_string()
                    } else {
                        format!("{} · {}", i18n::t(p.name), p.size())
                    };
                    let hover = if *have {
                        i18n::t(p.note).to_string()
                    } else {
                        format!(
                            "{}\n{}",
                            i18n::t(p.note),
                            tr!(K::PackNeedsDownload, p.size())
                        )
                    };
                    if ui
                        .selectable_label(p.code == cur.code, label)
                        .on_hover_text(hover)
                        .clicked()
                    {
                        chosen = Some(p);
                    }
                }
            });
        if let Some(p) = chosen {
            self.pick_ocr(ctx, p);
        }
    }

    /// 「简体中文 ▾」那一格
    fn lang_picker(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let cur = self.lang;
        let mut chosen = None;
        egui::ComboBox::from_id_salt("ui_lang")
            .width(120.0)
            .selected_text(cur.label())
            .show_ui(ui, |ui| {
                for l in Lang::ALL {
                    if ui.selectable_label(l == cur, l.label()).clicked() {
                        chosen = Some(l);
                    }
                }
            });
        if let Some(l) = chosen {
            self.set_lang(ctx, l);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _f: &mut eframe::Frame) {
        self.drain();
        self.poll_download();
        let ctx = root.ctx().clone();

        // 拖进来的文件
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() {
            self.add(dropped);
        }
        // 悬在窗口上方还没松手 —— 得给点反应, 不然人不知道这儿收不收
        let hovering = ctx.input(|i| i.raw.hovered_files.len());

        // 键盘: 回车开转, Esc 停 —— 队列摆好之后手还在键盘上, 不该被逼着去够鼠标
        let (enter, esc) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if enter && !self.running {
            self.start(&ctx);
        }
        if esc && self.running {
            self.stop.store(true, Ordering::Relaxed);
            self.stopping = true;
        }

        egui::Panel::top("bar").show(root, |ui| {
            enlarge(ui);
            let c = pal(ui);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.running, egui::Button::new(i18n::t(K::AddPdf)))
                    .clicked()
                {
                    if let Some(v) = rfd::FileDialog::new()
                        .add_filter("PDF", &["pdf"])
                        .pick_files()
                    {
                        self.add(v);
                    }
                }
                if ui
                    .add_enabled(
                        !self.running && !self.jobs.is_empty(),
                        egui::Button::new(i18n::t(K::Clear)),
                    )
                    .clicked()
                {
                    let all: Vec<usize> = (0..self.jobs.len()).collect();
                    self.remove(&all);
                }
                // 只在真有东西可撤时出现 —— 一个永远灰着的按钮只是占地方
                if !self.undo.is_empty()
                    && !self.running
                    && ui
                        .button(i18n::t(K::Undo))
                        .on_hover_text(i18n::t(K::UndoTip))
                        .clicked()
                {
                    let back = std::mem::take(&mut self.undo);
                    self.jobs.extend(back);
                }
                ui.separator();
                ui.label(i18n::t(K::DefaultOut))
                    .on_hover_text(i18n::t(K::DefaultOutTip));
                ui.selectable_value(&mut self.fmt, Format::Docx, "Word");
                ui.selectable_value(&mut self.fmt, Format::Xlsx, "Excel");
                ui.selectable_value(&mut self.fmt, Format::Both, i18n::t(K::FmtBoth));
                ui.separator();
                if ui
                    .button(if self.out_dir.is_some() {
                        i18n::t(K::OutDirChange)
                    } else {
                        i18n::t(K::OutDirPick)
                    })
                    .clicked()
                {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.out_dir = Some(d);
                    }
                }
                if self.out_dir.is_some()
                    && ui
                        .button("×")
                        .on_hover_text(i18n::t(K::OutDirReset))
                        .clicked()
                {
                    self.out_dir = None;
                }
                // 转完了想马上看看转成什么样 —— 让人自己顺着路径去翻文件夹是不体面的
                if let Some(d) = self.result_dir() {
                    if ui
                        .button(i18n::t(K::OpenResult))
                        .on_hover_text(d.display().to_string())
                        .clicked()
                    {
                        reveal(&d);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.running {
                        let label = if self.stopping {
                            i18n::t(K::Stopping)
                        } else {
                            i18n::t(K::Stop)
                        };
                        if ui
                            .add_enabled(!self.stopping, solid(label, c.halt))
                            .on_hover_text(i18n::t(K::StopTip))
                            .clicked()
                        {
                            self.stop.store(true, Ordering::Relaxed);
                            self.stopping = true;
                        }
                    } else if ui
                        .add_enabled(
                            !self.jobs.is_empty() && self.dl.is_none(),
                            solid(i18n::t(K::Start), c.cta),
                        )
                        .on_hover_text(i18n::t(K::StartTip))
                        .clicked()
                    {
                        self.start(&ctx);
                    }
                    ui.checkbox(&mut self.show_advanced, i18n::t(K::Advanced));
                });
            });
            ui.add_space(2.0);

            // 第二行专门放两种语言。它们不属于"高级参数": 选错识别语言不是
            // 调错一个阈值, 是整份文件一个字都认不出来
            ui.horizontal_wrapped(|ui| {
                ui.label(i18n::t(K::OcrLang))
                    .on_hover_text(i18n::t(K::OcrLangTip));
                ui.add_enabled_ui(!self.running && self.dl.is_none(), |ui| {
                    self.ocr_picker(ui, &ctx);
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.lang_picker(ui, &ctx);
                    ui.label(egui::RichText::new(i18n::t(K::UiLang)).small().color(c.dim));
                });
            });

            ui.label(
                egui::RichText::new(match &self.out_dir {
                    Some(d) => tr!(K::OutTo, d.display()),
                    None => i18n::t(K::OutToSrc).to_string(),
                })
                .small()
                .color(c.dim),
            );
            if self.show_advanced {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled(
                        !self.running,
                        egui::Slider::new(&mut self.cfg.long_edge, 1600..=4000)
                            .step_by(64.0)
                            .text(i18n::t(K::LongEdge)),
                    )
                    .on_hover_text(i18n::t(K::LongEdgeTip));
                    ui.checkbox(&mut self.cfg.text_layer, i18n::t(K::TextLayer))
                        .on_hover_text(i18n::t(K::TextLayerTip));
                    ui.checkbox(&mut self.cfg.deskew, i18n::t(K::Deskew))
                        .on_hover_text(i18n::t(K::DeskewTip));
                    ui.checkbox(&mut self.cfg.flatten, i18n::t(K::Flatten))
                        .on_hover_text(i18n::t(K::FlattenTip));
                    ui.checkbox(&mut self.cfg.keep_figures, i18n::t(K::Figures))
                        .on_hover_text(i18n::t(K::FiguresTip));
                    ui.checkbox(&mut self.cfg.grid_tables, i18n::t(K::GridTables));
                    ui.checkbox(&mut self.cfg.tables, i18n::t(K::Tables));
                    ui.checkbox(&mut self.cfg.page_marker, i18n::t(K::PageMarker));
                    ui.checkbox(&mut self.cfg.drop_header, i18n::t(K::DropHeader));
                    ui.checkbox(&mut self.cfg.drop_footer, i18n::t(K::DropFooter));
                    ui.checkbox(&mut self.cfg.drop_stamp, i18n::t(K::DropStamp));
                });
            }
            ui.add_space(4.0);
        });

        egui::Panel::bottom("progress").show(root, |ui| {
            let c = pal(ui);
            ui.add_space(4.0);
            if self.running {
                // 进度条按"整批"算而不是按当前这一份 —— 排了五个文件, 只看"第 3/96 页"
                // 判断不出还要等多久
                let (frac, note) = self.overall();
                ui.add(egui::ProgressBar::new(frac).text(note).desired_height(18.0));
            } else if let Some(d) = &self.dl {
                let pct = d.pct.load(Ordering::Relaxed);
                let note = tr!(K::PackDownloading, i18n::t(d.pack.name), pct);
                ui.add(
                    egui::ProgressBar::new(pct as f32 / 100.0)
                        .text(note)
                        .desired_height(18.0),
                );
            } else if let Some(s) = &self.summary {
                ui.label(egui::RichText::new(s).strong());
            } else {
                ui.label(egui::RichText::new(i18n::t(K::Ready)).color(c.dim));
            }
            ui.add_space(4.0);
        });

        egui::Panel::left("queue")
            .default_size(320.0)
            .show(root, |ui| {
                let c = pal(ui);
                ui.horizontal(|ui| {
                    ui.heading(i18n::t(K::Queue));
                    if !self.jobs.is_empty() {
                        ui.label(
                            egui::RichText::new(tr!(K::QueueCount, self.jobs.len())).color(c.dim),
                        );
                    }
                });
                ui.label(
                    egui::RichText::new(i18n::t(K::QueueHint))
                        .small()
                        .color(c.dim),
                );
                ui.separator();
                egui::ScrollArea::vertical().id_salt("q").show(ui, |ui| {
                    if self.jobs.is_empty() {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(i18n::t(K::QueueEmpty)).color(c.dim));
                    }
                    let mut drop_at = None;
                    let mut retry = None;
                    let mut open: Option<PathBuf> = None;
                    // 先取出来: 下面的循环把 self.jobs 借走了, 闭包里再读 self 会撞车
                    let (running, default_fmt) = (self.running, self.fmt);
                    for (i, j) in self.jobs.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            // 形状和颜色两条线都带着状态。只靠颜色分的话, 红绿色觉
                            // 障碍看到的就是四行一模一样的圆点
                            let (icon, color) = match &j.status {
                                Status::Waiting => ("○", c.idle),
                                Status::Running => ("▶", c.busy),
                                Status::Done { .. } => ("✓", c.ok),
                                Status::Failed(_) => ("✗", c.bad),
                            };
                            // 转好的那个 ✓ 是可以点的: 点开就是转出来的文件。这是转完
                            // 之后最想干的一件事, 不该让人拿着路径去文件管理器里翻
                            if let Status::Done { outs, .. } = &j.status {
                                let hit = ui
                                    .add(
                                        egui::Label::new(egui::RichText::new(icon).color(color))
                                            .sense(egui::Sense::click()),
                                    )
                                    // 一个能点的东西得看着能点。文字标签在 egui 里
                                    // 不会自己换指针, 光标不变的话没人会想到去点它
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_text(i18n::t(K::OpenOutTip));
                                if hit.clicked() {
                                    open = outs.first().cloned();
                                }
                            } else {
                                ui.colored_label(color, icon);
                            }
                            let name = j
                                .path
                                .file_name()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default();
                            // 右边的控件先摆: 剩多少宽度才轮到文件名, 名字再长也只是
                            // 被截断, 不会把格式选择框顶出可视区
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    // 不用 small_button: 那东西只有十来像素高,
                                    // 一个"删掉这一行"的靶子小到得瞄准就不该做
                                    if !running
                                        && ui
                                            .add(
                                                egui::Button::new("×")
                                                    .min_size(egui::vec2(24.0, 24.0)),
                                            )
                                            .on_hover_text(i18n::t(K::RemoveTip))
                                            .clicked()
                                    {
                                        drop_at = Some(i);
                                    }
                                    // 出了错的那一行给条现成的退路: 换个参数
                                    // 单独再来一遍, 不必把整队重跑
                                    if !running
                                        && matches!(j.status, Status::Failed(_))
                                        && ui
                                            .add(
                                                egui::Button::new("↻")
                                                    .min_size(egui::vec2(24.0, 24.0)),
                                            )
                                            .on_hover_text(i18n::t(K::RetryTip))
                                            .clicked()
                                    {
                                        retry = Some(i);
                                    }
                                    ui.add_enabled_ui(!running, |ui| {
                                        fmt_picker(ui, i, &mut j.fmt, default_fmt);
                                    });
                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            let r = ui.add(egui::Label::new(name).truncate());
                                            match &j.status {
                                                Status::Done { outs, .. } => {
                                                    let s: Vec<String> = outs
                                                        .iter()
                                                        .map(|p| p.display().to_string())
                                                        .collect();
                                                    r.on_hover_text(s.join("\n"));
                                                }
                                                Status::Failed(d) => {
                                                    r.on_hover_text(d);
                                                }
                                                _ => {
                                                    r.on_hover_text(j.path.display().to_string());
                                                }
                                            }
                                        },
                                    );
                                },
                            );
                        });
                        // 失败原因直接摆出来, 别藏在悬停提示里 —— 出了错正是最需要
                        // 一眼看见的时候
                        if let Status::Failed(m) = &j.status {
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                ui.label(egui::RichText::new(one_line(m, 60)).small().color(c.bad))
                                    .on_hover_text(m);
                            });
                        }
                    }
                    if let Some(i) = drop_at {
                        self.remove(&[i]);
                    }
                    if let Some(i) = retry {
                        self.start_some(&ctx, &[i], false);
                    }
                    if let Some(p) = open {
                        reveal(&p);
                    }
                });
            });

        egui::CentralPanel::default().show(root, |ui| {
            let c = pal(ui);
            ui.heading(i18n::t(K::LogTitle));
            ui.separator();
            // 头一回打开时这块是全空的, 一片空白不告诉人下一步该干什么。
            // 转过一轮(有了结论)之后就撤掉, 那时候日志本身才是要看的东西。
            // 注意是"摆在日志上面"而不是"代替日志": 添加时的提示(跳过了几个、
            // 为什么没加上)恰恰要在这个阶段看见。
            if !self.running && self.summary.is_none() {
                ui.add_space(6.0);
                for l in [K::Tip1, K::Tip2, K::Tip3] {
                    ui.label(egui::RichText::new(i18n::t(l)).color(c.dim));
                }
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(i18n::t(K::TipLocal))
                        .small()
                        .color(c.dim),
                );
                ui.separator();
            }
            egui::ScrollArea::vertical()
                .id_salt("log")
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for l in &self.log {
                        ui.label(egui::RichText::new(l).monospace().size(11.0));
                    }
                });
        });

        // 文件悬在窗口上方: 盖一层提示, 让人知道松手会发生什么
        if hovering > 0 {
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop_hint"),
            ));
            let r = ctx.content_rect();
            painter.rect_filled(r, 0.0, egui::Color32::from_black_alpha(160));
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                tr!(K::DropOverlay, hovering),
                egui::FontId::proportional(24.0),
                egui::Color32::WHITE,
            );
        }

        if self.running || self.dl.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }
}

fn is_pdf(p: &std::path::Path) -> bool {
    p.extension().map(|e| e.eq_ignore_ascii_case("pdf")) == Some(true)
}

/// 把目录里的 PDF 都翻出来
///
/// 限制深度 6: 拖进来的要是家目录, 无限递归会让界面卡住不动, 看着像死机。
/// 跳过隐藏目录, 免得把 .pdf2doc_cache 之类的东西也扫进来。
fn collect_pdfs(dir: &std::path::Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 6 || out.len() > 5000 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let hidden = p
            .file_name()
            .map(|s| s.to_string_lossy().starts_with('.'))
            .unwrap_or(false);
        if hidden {
            continue;
        }
        if p.is_dir() {
            collect_pdfs(&p, depth + 1, out);
        } else if is_pdf(&p) {
            out.push(p);
        }
    }
}

/// 在文件管理器里定位一个文件/目录
///
/// 不用 open 之类的 crate: 三行命令的事, 不值得为它多一条依赖和一份授权要交代。
fn reveal(p: &std::path::Path) {
    let _ = if cfg!(target_os = "macos") {
        // -R 是"在访达里选中它", 比直接打开文件更合适: 同一次转换往往出两份
        std::process::Command::new("open").arg("-R").arg(p).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", p.display()))
            .spawn()
    } else {
        let d = if p.is_dir() {
            p
        } else {
            p.parent().unwrap_or(p)
        };
        std::process::Command::new("xdg-open").arg(d).spawn()
    };
}

/// 秒数写成人看的样子
fn dur(s: u64) -> String {
    match s {
        0..=59 => tr!(K::DurS, s),
        60..=3599 => tr!(K::DurM, s / 60, s % 60),
        _ => tr!(K::DurH, s / 3600, (s % 3600) / 60),
    }
}

/// 挤成一行并截断 —— 错误信息里常有换行, 直接摆进队列里会把行高撑开
fn one_line(s: &str, max: usize) -> String {
    let t: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= max {
        return t;
    }
    t.chars().take(max).collect::<String>() + "…"
}

/// 把顶部操作区的控件放大一档
///
/// 那一排是每次都要点的东西(添加、选格式、开始转换), egui 默认的按钮只有二十来
/// 像素高, 在高分屏上点着费劲。改的是这个 ui 子树的 style, 子控件继承 —— 队列和
/// 日志区不受影响, 那两处是看的不是点的, 一起放大只会少显示几行。
///
/// Small 那一档不动: 「输出到 …」那行提示还是小字, 不然整条工具栏喧宾夺主。
fn enlarge(ui: &mut egui::Ui) {
    let st = ui.style_mut();
    st.text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(16.0));
    st.text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
    st.spacing.button_padding = egui::vec2(12.0, 7.0);
    st.spacing.item_spacing = egui::vec2(8.0, 6.0);
    st.spacing.interact_size.y = 30.0;
    // 复选框那个方块跟着字一起长, 不然放大后按钮旁边那个勾小得突兀
    st.spacing.icon_width = 18.0;
    st.spacing.icon_width_inner = 10.0;
    st.spacing.icon_spacing = 6.0;
}

fn fmt_name(f: Format) -> &'static str {
    match f {
        Format::Docx => "Word",
        Format::Xlsx => "Excel",
        Format::Both => i18n::t(K::FmtBothShort),
    }
}

/// 队列里每行的格式选择框
///
/// 「默认」是一档实实在在的选项, 不是"没选": 选着它的文件跟着顶栏一起变,
/// 一队文件想统一换格式点一下顶栏就行。
fn fmt_picker(ui: &mut egui::Ui, id: usize, fmt: &mut Option<Format>, default: Format) {
    let shown = match fmt {
        None => tr!(K::FmtDefault, fmt_name(default)),
        Some(f) => fmt_name(*f).to_string(),
    };
    egui::ComboBox::from_id_salt(("fmt", id))
        .width(88.0)
        .selected_text(egui::RichText::new(shown).size(11.0))
        .show_ui(ui, |ui| {
            ui.selectable_value(fmt, None, tr!(K::FmtDefault, fmt_name(default)));
            for f in [Format::Docx, Format::Xlsx, Format::Both] {
                ui.selectable_value(fmt, Some(f), fmt_name(f));
            }
        });
}

/// 常驻工作线程: 一批批地取任务, Converter 只建一次
///
/// 每批都新建的代价不只是重载 32 MB 模型(一两秒): pdfium 的绑定在
/// pdfium-render 里是进程级全局的, 重复初始化会直接报错。换识别语言时也是
/// 同理 —— 只换 OCR 那半边(`reload_ocr`), 渲染器留着不动。
fn worker(rx: Receiver<Batch>) {
    let mut conv: Option<Converter> = None;
    let mut models: Option<PathBuf> = None;
    let mut cur_rec: Rec = None;
    // 主线程退出时 job_tx 被丢掉, 这个循环随之结束
    for b in rx {
        if let Err(e) = ready(&mut conv, &mut models, &mut cur_rec, &b.rec) {
            let _ = b.tx.send(Msg::Log(tr!(K::InitFailed, format!("{e:#}"))));
            let _ = b.tx.send(Msg::AllDone);
            b.ctx.request_repaint();
            continue;
        }
        run_batch(conv.as_mut().expect("上面刚建好"), &b);
    }
}

/// 建引擎, 或者换掉识别模型
fn ready(
    conv: &mut Option<Converter>,
    models: &mut Option<PathBuf>,
    cur: &mut Rec,
    want: &Rec,
) -> anyhow::Result<()> {
    let dir = match models {
        Some(d) => d.clone(),
        None => {
            let d = locate_models()?;
            *models = Some(d.clone());
            d
        }
    };
    let opts = engine_opts(want);
    match conv.as_mut() {
        None => *conv = Some(Converter::new_with(&dir, opts)?),
        Some(c) if cur != want => c.reload_ocr(&dir, opts)?,
        Some(_) => {}
    }
    *cur = want.clone();
    Ok(())
}

fn engine_opts(rec: &Rec) -> EngineOptions {
    match rec {
        None => EngineOptions::default(),
        Some((dir, file)) => EngineOptions {
            rec_dir: Some(dir.clone()),
            rec_file: Some(file.clone()),
            ..EngineOptions::default()
        },
    }
}

/// 转完一批文件, 逐份回报进度与结果
fn run_batch(conv: &mut Converter, b: &Batch) {
    let Batch {
        tasks,
        cfg,
        out,
        tx,
        stop,
        ctx,
        ..
    } = b;
    let say = |m: String| {
        let _ = tx.send(Msg::Log(m));
        ctx.request_repaint();
    };

    for (i, p, fmt) in tasks.iter() {
        let i = *i;
        if stop.load(Ordering::Relaxed) {
            say(i18n::t(K::Stopped).into());
            break;
        }
        say(format!("=== {} [{}] ===", p.display(), fmt_name(*fmt)));
        let log = |m: &str| {
            let _ = tx.send(Msg::Log(m.to_string()));
        };
        let prog = |_s: Stage, cur: usize, total: usize, m: &str| {
            let _ = tx.send(Msg::Progress {
                job: i,
                cur,
                total,
                text: format!("{}/{} {m}", cur, total),
            });
            ctx.request_repaint();
        };
        let halt = || stop.load(Ordering::Relaxed);
        let hooks = Hooks {
            progress: Some(&prog),
            log: Some(&log),
            stop: Some(&halt),
        };

        let status = match conv.convert(p, out.as_deref(), cfg, *fmt, &hooks) {
            Ok(r) => Status::Done {
                outs: r.outputs,
                warn: (!r.errors.is_empty()).then(|| tr!(K::LibFileErrors, r.errors.len())),
            },
            Err(e) => {
                let m = format!("{e:#}");
                say(tr!(K::ConvFailed, m));
                Status::Failed(m)
            }
        };
        let _ = tx.send(Msg::Job { job: i, status });
        ctx.request_repaint();
    }
    let _ = tx.send(Msg::AllDone);
    ctx.request_repaint();
}

/// 装一个能显示当前界面语言的字体 —— egui 自带的字体只有拉丁字母
///
/// 用系统字体而不是把字体打进包里: 一个 CJK 字库动辄 10~20 MB, 而这版的全部
/// 卖点就是小。
///
/// 韩语要单独排一批候选: 汉字字体(PingFang、STHeiti、宋体)一个谚文都没有,
/// 而队列里的文件名又可能是中文, 所以韩语界面下两批都装 —— 谚文的排在前面,
/// 汉字的垫在后面。其余六种语言用同一批就够了, 日文假名在中文字体里是全的。
fn install_ui_font(ctx: &egui::Context, lang: Lang) {
    // (路径, ttc 里第几个字体)
    const CJK: &[(&str, u32)] = &[
        // macOS。按体积从小到大挑 —— 都是整个读进内存的
        ("/System/Library/Fonts/PingFang.ttc", 2),
        ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
        ("/System/Library/Fonts/STHeiti Light.ttc", 0),
        ("/System/Library/Fonts/Supplemental/Songti.ttc", 0),
        // Windows
        ("C:\\Windows\\Fonts\\msyh.ttc", 0),
        ("C:\\Windows\\Fonts\\simhei.ttf", 0),
        ("C:\\Windows\\Fonts\\simsun.ttc", 0),
        // Linux
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
        ("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc", 0),
        ("/usr/share/fonts/truetype/arphic/uming.ttc", 0),
    ];
    const HANGUL: &[(&str, u32)] = &[
        // AppleGothic 15 MB, AppleSDGothicNeo 55 MB —— 先试小的
        ("/System/Library/Fonts/Supplemental/AppleGothic.ttf", 0),
        ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0),
        ("C:\\Windows\\Fonts\\malgun.ttf", 0),
        ("C:\\Windows\\Fonts\\gulim.ttc", 0),
        ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
        ("/usr/share/fonts/truetype/nanum/NanumGothic.ttf", 0),
    ];

    let mut fonts = egui::FontDefinitions::default();
    let mut names: Vec<String> = Vec::new();
    // 韩语界面先装谚文那批, 汉字那批垫在后面当兜底
    let lists: &[&[(&str, u32)]] = if lang == Lang::Ko {
        &[HANGUL, CJK]
    } else {
        &[CJK]
    };
    for (n, list) in lists.iter().enumerate() {
        for (path, index) in *list {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let mut data = egui::FontData::from_owned(bytes);
            data.index = *index;
            let key = format!("ui{n}");
            fonts.font_data.insert(key.clone(), Arc::new(data));
            names.push(key);
            break;
        }
    }
    if names.is_empty() {
        return;
    }
    // 放在各字族末尾当兜底: 拉丁字形还用 egui 自带的, 排版好看些
    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let e = fonts.families.entry(fam).or_default();
        for n in &names {
            e.push(n.clone());
        }
    }
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result {
    // 语言要在建窗口之前定下来 —— 窗口标题就是第一句要翻的话
    let st = Settings::load();
    i18n::set(
        st.lang
            .as_deref()
            .and_then(Lang::parse)
            .unwrap_or_else(i18n::detect),
    );

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1040.0, 680.0])
            // 工具栏放大后一行实测要 771 px(选了输出目录会再多几十), 最小宽度低于
            // 这个数右边的「开始转换」就被挤出可视区了
            .with_min_inner_size([880.0, 520.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        i18n::t(K::WindowTitle),
        opts,
        Box::new(|cc| Ok(Box::new(App::new(cc, st)))),
    )
}
