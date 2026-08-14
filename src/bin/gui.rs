//! 图形界面: 一个批量转换队列 + 一块参数面板
//!
//! 转换跑在工作线程上, 通过 channel 把进度和日志送回来 —— 主线程只管画。
//! Converter 在工作线程内部创建, 从不跨线程传递, 省得跟 ort / pdfium 的
//! Send 界限较劲。
//!
//! 工作线程是**常驻**的, 一批批任务从 channel 里取: Converter 因此只建一次。
//! 每批都新建的话, 32 MB 模型要重载一遍(一两秒), pdfium 的进程级绑定也会被
//! 重复初始化。

#![cfg_attr(windows, windows_subsystem = "windows")]

use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use scannedpdf2doc::config::{Config, Format};
use scannedpdf2doc::{Converter, Hooks, Stage};

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

/// 一批任务: 主线程攒好丢给常驻工作线程
struct Batch {
    /// 每份文件连着它自己的输出格式 —— 默认已经在主线程里解开了
    tasks: Vec<(PathBuf, Format)>,
    cfg: Config,
    out: Option<PathBuf>,
    tx: Sender<Msg>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
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
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_font(&cc.egui_ctx);
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
        }
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
                    self.log.push(format!("{} 里没有 PDF", p.display()));
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
        let mut note = format!("加入 {added} 个文件");
        if dup > 0 {
            note += &format!(", 跳过 {dup} 个已在队列里的");
        }
        if skipped > 0 {
            note += &format!(", 跳过 {skipped} 个不是 PDF 的");
        }
        if self.running && added > 0 {
            // 这一批的任务清单在点开始的那一刻就定死了, 现在加的赶不上
            note += " —— 正在转换中, 这些要等下一轮";
        }
        if added + dup + skipped > 0 {
            self.log.push(note);
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        if self.running || self.jobs.is_empty() {
            return;
        }
        for j in self.jobs.iter_mut() {
            j.status = Status::Waiting;
        }
        self.log.clear();
        self.running = true;
        self.stopping = false;
        self.summary = None;
        self.started = Some(std::time::Instant::now());
        self.stop = Arc::new(AtomicBool::new(false));

        let (tx, rx) = channel();
        self.rx = Some(rx);
        let batch = Batch {
            tasks: self
                .jobs
                .iter()
                .map(|j| (j.path.clone(), j.fmt.unwrap_or(self.fmt)))
                .collect(),
            cfg: self.cfg.clone(),
            out: self.out_dir.clone(),
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
            self.log.push("工作线程已退出, 请重启程序".into());
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
        let n = self.jobs.len().max(1);
        let done = self
            .jobs
            .iter()
            .filter(|j| matches!(j.status, Status::Done { .. } | Status::Failed(_)))
            .count();
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
        let mut s = format!("({}/{}) {name}", (self.cur.job + 1).min(n), n);
        if !self.cur.text.is_empty() {
            s += &format!(" · {}", self.cur.text);
        }
        if let Some(t) = self.started {
            let el = t.elapsed().as_secs();
            s += &format!(" · 已用 {}", dur(el));
            // 头 5% 的外推纯属瞎猜, 别显示
            if frac > 0.05 {
                s += &format!(", 约剩 {}", dur((el as f32 * (1.0 - frac) / frac) as u64));
            }
        }
        (frac, s)
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
                    let ok = self
                        .jobs
                        .iter()
                        .filter(|j| matches!(j.status, Status::Done { .. }))
                        .count();
                    let bad = self
                        .jobs
                        .iter()
                        .filter(|j| matches!(j.status, Status::Failed(_)))
                        .count();
                    let left = self.jobs.len() - ok - bad;
                    let mut s = format!("完成 {ok} 个");
                    if bad > 0 {
                        s += &format!(", 失败 {bad} 个");
                    }
                    if left > 0 {
                        s += &format!(", 未转 {left} 个");
                    }
                    if let Some(t) = self.started {
                        s += &format!(" · 用时 {}", dur(t.elapsed().as_secs()));
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
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _f: &mut eframe::Frame) {
        self.drain();
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
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.running, egui::Button::new("添加 PDF…"))
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
                    .add_enabled(!self.running, egui::Button::new("清空"))
                    .clicked()
                {
                    self.jobs.clear();
                }
                ui.separator();
                ui.label("默认输出:")
                    .on_hover_text("队列里可以给单个文件另设格式, 没设的都跟这里走");
                ui.selectable_value(&mut self.fmt, Format::Docx, "Word");
                ui.selectable_value(&mut self.fmt, Format::Xlsx, "Excel");
                ui.selectable_value(&mut self.fmt, Format::Both, "两份都要");
                ui.separator();
                if ui
                    .button(if self.out_dir.is_some() {
                        "改输出目录"
                    } else {
                        "输出目录…"
                    })
                    .clicked()
                {
                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                        self.out_dir = Some(d);
                    }
                }
                if self.out_dir.is_some()
                    && ui.button("×").on_hover_text("改回源文件同目录").clicked()
                {
                    self.out_dir = None;
                }
                // 转完了想马上看看转成什么样 —— 让人自己顺着路径去翻文件夹是不体面的
                if let Some(d) = self.result_dir() {
                    if ui
                        .button("打开结果")
                        .on_hover_text(d.display().to_string())
                        .clicked()
                    {
                        reveal(&d);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.running {
                        let label = if self.stopping {
                            "正在停止…"
                        } else {
                            "停止"
                        };
                        if ui
                            .add_enabled(!self.stopping, egui::Button::new(label))
                            .on_hover_text("Esc")
                            .clicked()
                        {
                            self.stop.store(true, Ordering::Relaxed);
                            self.stopping = true;
                        }
                    } else if ui
                        .add_enabled(!self.jobs.is_empty(), egui::Button::new("开始转换"))
                        .on_hover_text("回车")
                        .clicked()
                    {
                        self.start(&ctx);
                    }
                    ui.checkbox(&mut self.show_advanced, "高级参数");
                });
            });
            ui.label(
                egui::RichText::new(match &self.out_dir {
                    Some(d) => format!("输出到 {}", d.display()),
                    None => "输出到源文件所在目录".into(),
                })
                .small()
                .weak(),
            );
            if self.show_advanced {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled(
                        !self.running,
                        egui::Slider::new(&mut self.cfg.long_edge, 1600..=4000)
                            .step_by(64.0)
                            .text("渲染长边像素"),
                    )
                    .on_hover_text("实际 dpi 由页面尺寸倒推并夹在 150~300; 调小识别快但小字容易丢");
                    ui.checkbox(&mut self.cfg.grid_tables, "按框线还原表格");
                    ui.checkbox(&mut self.cfg.tables, "还原无框线多列版面");
                    ui.checkbox(&mut self.cfg.page_marker, "插「原第 N 页」标记");
                    ui.checkbox(&mut self.cfg.drop_header, "去页眉");
                    ui.checkbox(&mut self.cfg.drop_footer, "去页脚");
                    ui.checkbox(&mut self.cfg.drop_stamp, "去印章噪声");
                });
            }
            ui.add_space(4.0);
        });

        egui::Panel::bottom("progress").show(root, |ui| {
            ui.add_space(4.0);
            if self.running {
                // 进度条按"整批"算而不是按当前这一份 —— 排了五个文件, 只看"第 3/96 页"
                // 判断不出还要等多久
                let (frac, note) = self.overall();
                ui.add(egui::ProgressBar::new(frac).text(note).desired_height(18.0));
            } else if let Some(s) = &self.summary {
                ui.label(egui::RichText::new(s).strong());
            } else {
                ui.label(egui::RichText::new("就绪").weak());
            }
            ui.add_space(4.0);
        });

        egui::Panel::left("queue")
            .default_size(320.0)
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("队列");
                    if !self.jobs.is_empty() {
                        ui.label(egui::RichText::new(format!("{} 个", self.jobs.len())).weak());
                    }
                });
                ui.label(
                    egui::RichText::new("PDF 或整个文件夹, 拖进窗口就行")
                        .small()
                        .weak(),
                );
                ui.separator();
                egui::ScrollArea::vertical().id_salt("q").show(ui, |ui| {
                    if self.jobs.is_empty() {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("队列是空的").weak());
                    }
                    let mut drop_at = None;
                    let mut open: Option<PathBuf> = None;
                    // 先取出来: 下面的循环把 self.jobs 借走了, 闭包里再读 self 会撞车
                    let (running, default_fmt) = (self.running, self.fmt);
                    for (i, j) in self.jobs.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            let (icon, color) = match &j.status {
                                Status::Waiting => ("○", egui::Color32::GRAY),
                                Status::Running => ("▶", egui::Color32::from_rgb(0x2a, 0x7a, 0xe0)),
                                Status::Done { .. } => {
                                    ("✓", egui::Color32::from_rgb(0x2a, 0x9a, 0x4a))
                                }
                                Status::Failed(_) => {
                                    ("✗", egui::Color32::from_rgb(0xc0, 0x30, 0x30))
                                }
                            };
                            // 转好的那个 ✓ 是可以点的: 点开就是转出来的文件。这是转完
                            // 之后最想干的一件事, 不该让人拿着路径去文件管理器里翻
                            if let Status::Done { outs, .. } = &j.status {
                                let hit = ui
                                    .add(
                                        egui::Label::new(egui::RichText::new(icon).color(color))
                                            .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text("点开转出来的文件");
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
                                    if !running && ui.small_button("×").clicked() {
                                        drop_at = Some(i);
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
                                ui.label(
                                    egui::RichText::new(one_line(m, 60))
                                        .small()
                                        .color(egui::Color32::from_rgb(0xc0, 0x30, 0x30)),
                                )
                                .on_hover_text(m);
                            });
                        }
                    }
                    if let Some(i) = drop_at {
                        self.jobs.remove(i);
                    }
                    if let Some(p) = open {
                        reveal(&p);
                    }
                });
            });

        egui::CentralPanel::default().show(root, |ui| {
            ui.heading("日志");
            ui.separator();
            // 头一回打开时这块是全空的, 一片空白不告诉人下一步该干什么。
            // 转过一轮(有了结论)之后就撤掉, 那时候日志本身才是要看的东西。
            // 注意是"摆在日志上面"而不是"代替日志": 添加时的提示(跳过了几个、
            // 为什么没加上)恰恰要在这个阶段看见。
            if !self.running && self.summary.is_none() {
                ui.add_space(6.0);
                for l in [
                    "① 把扫描版 PDF 拖进左边, 或点「添加 PDF…」——整个文件夹也行",
                    "② 选输出格式: Word 保版式, Excel 只导表格; 队列里可以单独给某份另设",
                    "③ 点「开始转换」(或直接敲回车)",
                ] {
                    ui.label(egui::RichText::new(l).weak());
                }
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "全程在本机跑, 不联网, 文件不出这台电脑; 首次转换要加载模型, 慢一两秒",
                    )
                    .small()
                    .weak(),
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
                format!("松手加入队列 ({hovering} 项)"),
                egui::FontId::proportional(24.0),
                egui::Color32::WHITE,
            );
        }

        if self.running {
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
        0..=59 => format!("{s} 秒"),
        60..=3599 => format!("{} 分 {} 秒", s / 60, s % 60),
        _ => format!("{} 小时 {} 分", s / 3600, (s % 3600) / 60),
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
        Format::Both => "两份",
    }
}

/// 队列里每行的格式选择框
///
/// 「默认」是一档实实在在的选项, 不是"没选": 选着它的文件跟着顶栏一起变,
/// 一队文件想统一换格式点一下顶栏就行。
fn fmt_picker(ui: &mut egui::Ui, id: usize, fmt: &mut Option<Format>, default: Format) {
    let shown = match fmt {
        None => format!("默认·{}", fmt_name(default)),
        Some(f) => fmt_name(*f).to_string(),
    };
    egui::ComboBox::from_id_salt(("fmt", id))
        .width(78.0)
        .selected_text(egui::RichText::new(shown).size(11.0))
        .show_ui(ui, |ui| {
            ui.selectable_value(fmt, None, format!("默认·{}", fmt_name(default)));
            for f in [Format::Docx, Format::Xlsx, Format::Both] {
                ui.selectable_value(fmt, Some(f), fmt_name(f));
            }
        });
}

/// 常驻工作线程: 一批批地取任务, Converter 只建一次
///
/// 每批都新建的代价不只是重载 32 MB 模型(一两秒): pdfium 的绑定在
/// pdfium-render 里是进程级全局的, 重复初始化会直接报错。
fn worker(rx: Receiver<Batch>) {
    let mut conv: Option<Converter> = None;
    // 主线程退出时 job_tx 被丢掉, 这个循环随之结束
    for b in rx {
        if conv.is_none() {
            match Converter::with_default_models() {
                Ok(c) => conv = Some(c),
                Err(e) => {
                    let _ = b.tx.send(Msg::Log(format!("初始化失败: {e:#}")));
                    let _ = b.tx.send(Msg::AllDone);
                    b.ctx.request_repaint();
                    continue;
                }
            }
        }
        run_batch(conv.as_mut().expect("上面刚建好"), &b);
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
    } = b;
    let say = |m: String| {
        let _ = tx.send(Msg::Log(m));
        ctx.request_repaint();
    };

    for (i, (p, fmt)) in tasks.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            say("已停止".into());
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
                warn: (!r.errors.is_empty()).then(|| format!("{} 页失败, 已跳过", r.errors.len())),
            },
            Err(e) => {
                let m = format!("{e:#}");
                say(format!("  ✗ 转换失败: {m}"));
                Status::Failed(m)
            }
        };
        let _ = tx.send(Msg::Job { job: i, status });
        ctx.request_repaint();
    }
    let _ = tx.send(Msg::AllDone);
    ctx.request_repaint();
}

/// 装一个中文字体 —— egui 自带的字体没有汉字, 不装满界面都是豆腐块
///
/// 用系统字体而不是把字体打进包里: 一个中文字库动辄 10~20 MB, 而这版的全部
/// 卖点就是小。
fn install_cjk_font(ctx: &egui::Context) {
    const CANDS: &[(&str, u32)] = &[
        // macOS
        ("/System/Library/Fonts/PingFang.ttc", 2),
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
    for (path, index) in CANDS {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        let mut data = egui::FontData::from_owned(bytes);
        data.index = *index;
        fonts.font_data.insert("cjk".into(), Arc::new(data));
        // 放在各字族末尾当兜底: 拉丁字形还用 egui 自带的, 排版好看些
        for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().push("cjk".into());
        }
        ctx.set_fonts(fonts);
        return;
    }
}

fn main() -> eframe::Result {
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
        "扫描件 PDF → Word / Excel",
        opts,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
