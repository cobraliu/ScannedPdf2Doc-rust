//! 命令行入口

use clap::{CommandFactory, FromArgMatches, Parser};
use std::path::PathBuf;

use scannedpdf2doc::config::{Config, Format};
use scannedpdf2doc::i18n::{self, Lang, K};
use scannedpdf2doc::ocr::EngineOptions;
use scannedpdf2doc::ocrlang;
use scannedpdf2doc::{tr, Converter, Hooks};

// 帮助文本里的 doc comment 一律留空: 真正的说明在运行时按界面语言塞进去
// (见 `parse_localized`)。写成中文再覆盖只会让两处不同步。
#[derive(Parser)]
#[command(name = "pdf2doc", version)]
struct Args {
    pdfs: Vec<PathBuf>,

    #[arg(long, default_value = "docx")]
    to: String,

    #[arg(short, long)]
    out: Option<PathBuf>,

    #[arg(long, default_value_t = 2560)]
    long_edge: u32,

    #[arg(long)]
    no_tables: bool,

    #[arg(long)]
    no_grid: bool,

    #[arg(long)]
    no_marker: bool,

    #[arg(long)]
    no_text_layer: bool,

    #[arg(long)]
    no_deskew: bool,

    #[arg(long)]
    models: Option<PathBuf>,

    #[arg(short, long)]
    quiet: bool,

    /// 界面语言
    #[arg(long)]
    lang: Option<String>,

    /// 识别语言
    #[arg(long)]
    ocr_lang: Option<String>,
}

/// `--lang` 认得的那几个值, 拼成一行给报错和帮助用
fn lang_tags() -> String {
    Lang::ALL
        .iter()
        .map(|l| l.tag())
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 先把界面语言定下来, 再按它渲染 `--help`
///
/// 有个先有鸡还是先有蛋: `--help` 要用哪种语言写, 取决于同一行里的 `--lang`,
/// 而 clap 在解析出 `--lang` 的同时就已经把帮助打出去了。所以先手工从
/// argv 里捞一眼 `--lang`, 定好语言, 再把说明文字挂到 clap 的 Command 上。
fn parse_localized() -> Args {
    let argv: Vec<String> = std::env::args().collect();
    let raw = argv.iter().enumerate().find_map(|(i, a)| {
        a.strip_prefix("--lang=")
            .map(str::to_string)
            .or_else(|| (a == "--lang").then(|| argv.get(i + 1).cloned()).flatten())
    });
    // 先按系统语言定一次: `--lang` 给错了的时候, 那句报错也得有语言可用
    i18n::set(i18n::detect());
    if let Some(s) = raw.as_deref() {
        match Lang::parse(s) {
            Some(l) => i18n::set(l),
            // 这儿还没有 clap 的错误机制可用, 自己报
            None => {
                eprintln!("{}", tr!(K::CliBadLang, lang_tags(), s));
                std::process::exit(2);
            }
        }
    }

    let help_lang = format!("{} [{}]", i18n::t(K::HelpLang), lang_tags());
    let help_ocr = format!("{} [{}]", i18n::t(K::HelpOcrLang), ocrlang::codes());
    let cmd = Args::command()
        .about(i18n::t(K::CliAbout))
        .mut_arg("pdfs", |a| a.help(i18n::t(K::HelpPdfs)))
        .mut_arg("to", |a| a.help(i18n::t(K::HelpTo)))
        .mut_arg("out", |a| a.help(i18n::t(K::HelpOut)))
        .mut_arg("long_edge", |a| a.help(i18n::t(K::HelpLongEdge)))
        .mut_arg("no_tables", |a| a.help(i18n::t(K::HelpNoTables)))
        .mut_arg("no_grid", |a| a.help(i18n::t(K::HelpNoGrid)))
        .mut_arg("no_marker", |a| a.help(i18n::t(K::HelpNoMarker)))
        .mut_arg("no_text_layer", |a| a.help(i18n::t(K::HelpNoTextLayer)))
        .mut_arg("no_deskew", |a| a.help(i18n::t(K::HelpNoDeskew)))
        .mut_arg("models", |a| a.help(i18n::t(K::HelpModels)))
        .mut_arg("quiet", |a| a.help(i18n::t(K::HelpQuiet)))
        .mut_arg("lang", |a| a.help(help_lang))
        .mut_arg("ocr_lang", |a| a.help(help_ocr));
    Args::from_arg_matches(&cmd.get_matches()).unwrap_or_else(|e| e.exit())
}

fn main() {
    let a = parse_localized();
    if a.pdfs.is_empty() {
        eprintln!("{}", i18n::t(K::CliUsage));
        std::process::exit(2);
    }
    let Some(fmt) = Format::parse(&a.to) else {
        eprintln!("{}", tr!(K::CliBadFormat, format!("{:?}", a.to)));
        std::process::exit(2);
    };

    // 缺语言包就现下 —— 命令行下这一步要在真正干活之前做完, 不然一批文件
    // 转到一半才发现认不了字, 前面那些白转了
    let pack = match &a.ocr_lang {
        None => &ocrlang::PACKS[0],
        Some(c) => {
            let Some(p) = ocrlang::PACKS.iter().find(|p| !p.builtin() && p.code == c) else {
                eprintln!("{}", tr!(K::CliBadOcrLang, ocrlang::codes(), c));
                std::process::exit(2);
            };
            p
        }
    };
    if !pack.installed() {
        // 只在整十的位置报一次, 不然重定向到文件里全是进度行
        let step = std::cell::Cell::new(0u32);
        let tick = |v: u32| {
            if v / 10 > step.get() {
                step.set(v / 10);
                println!("{}", tr!(K::PackDownloading, i18n::t(pack.name), v));
            }
        };
        if let Err(e) = ocrlang::prepare(pack, &tick) {
            eprintln!(
                "{}",
                tr!(K::PackFailed, i18n::t(pack.name), format!("{e:#}"))
            );
            std::process::exit(1);
        }
    }

    let cfg = Config {
        long_edge: a.long_edge,
        tables: !a.no_tables,
        grid_tables: !a.no_grid,
        page_marker: !a.no_marker,
        text_layer: !a.no_text_layer,
        deskew: !a.no_deskew,
        ..Default::default()
    };

    // 逐页统计默认打出来, --quiet 时只留结果行(以 ✓ / ! / ↷ 开头的)
    // ↷ 是"同名文件已存在, 改存成别的名字"—— 这是结果的一部分, 静默模式也得说
    let quiet = a.quiet;
    let log = move |m: &str| {
        if !quiet || m.trim_start().starts_with(['✓', '!', '↷']) {
            println!("{m}");
        }
    };
    let hooks = Hooks {
        log: Some(&log),
        ..Default::default()
    };

    let opts = EngineOptions {
        rec_dir: ocrlang::installed_dir(pack),
        rec_file: (!pack.builtin()).then(|| pack.file.to_string()),
        ..Default::default()
    };
    let dir = match a.models {
        Some(d) => Ok(d),
        None => scannedpdf2doc::locate_models(),
    };
    let mut conv = dir
        .and_then(|d| Converter::new_with(&d, opts))
        .unwrap_or_else(|e| {
            eprintln!("{}", tr!(K::InitFailed, format!("{e:#}")));
            std::process::exit(1);
        });

    let mut failed = Vec::new();
    let mut page_errs = Vec::new();
    for p in &a.pdfs {
        match conv.convert(p, a.out.as_deref(), &cfg, fmt, &hooks) {
            // 整篇失败也不退出: 记下来继续下一个文件
            Err(e) => {
                println!("{}", tr!(K::CliConvFailed, p.display(), format!("{e:#}")));
                failed.push(p.clone());
            }
            Ok(r) => {
                for e in r.errors {
                    page_errs.push((p.clone(), e));
                }
            }
        }
    }
    if !failed.is_empty() || !page_errs.is_empty() {
        println!("\n{}", i18n::t(K::CliErrSummary));
        for p in &failed {
            println!("{}", tr!(K::CliWholeFailed, p.display()));
        }
        for (p, e) in &page_errs {
            let f = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            println!("{}", tr!(K::CliPageFailed, f, e.page, e.stage, e.msg));
        }
        std::process::exit(1);
    }
}
