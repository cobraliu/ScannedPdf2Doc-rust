//! 命令行入口

use clap::Parser;
use std::path::PathBuf;

use scannedpdf2doc::config::{Config, Format};
use scannedpdf2doc::{Converter, Hooks};

#[derive(Parser)]
#[command(
    name = "pdf2doc",
    about = "扫描版 PDF -> 保版式可编辑 Word / Excel 表格 (全本地, 无需联网)",
    version
)]
struct Args {
    /// 待转换的扫描版 PDF
    pdfs: Vec<PathBuf>,

    /// 输出格式: docx(默认, 保版式) / xlsx(只导表格) / both
    #[arg(long, default_value = "docx")]
    to: String,

    /// 输出目录, 默认与源文件同目录
    #[arg(short, long)]
    out: Option<PathBuf>,

    /// 渲染长边像素, 越大越准也越慢(实际 dpi 由页面尺寸倒推, 夹在 150~300)
    #[arg(long, default_value_t = 2560)]
    long_edge: u32,

    /// 不还原无框线的多列版面
    #[arg(long)]
    no_tables: bool,

    /// 不按框线还原表格
    #[arg(long)]
    no_grid: bool,

    /// 不插「原第 N 页」标记
    #[arg(long)]
    no_marker: bool,

    /// OCR 模型目录, 默认自动查找
    #[arg(long)]
    models: Option<PathBuf>,

    /// 只打结果, 不打逐页统计
    #[arg(short, long)]
    quiet: bool,
}

fn main() {
    let a = Args::parse();
    if a.pdfs.is_empty() {
        eprintln!("用法: pdf2doc <文件.pdf> [...]  (更多选项见 --help)");
        std::process::exit(2);
    }
    let Some(fmt) = Format::parse(&a.to) else {
        eprintln!("--to 只能是 docx / xlsx / both, 收到 {:?}", a.to);
        std::process::exit(2);
    };

    let cfg = Config {
        long_edge: a.long_edge,
        tables: !a.no_tables,
        grid_tables: !a.no_grid,
        page_marker: !a.no_marker,
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

    let mut conv = match a.models {
        Some(d) => Converter::new(&d),
        None => Converter::with_default_models(),
    }
    .unwrap_or_else(|e| {
        eprintln!("初始化失败: {e:#}");
        std::process::exit(1);
    });

    let mut failed = Vec::new();
    let mut page_errs = Vec::new();
    for p in &a.pdfs {
        match conv.convert(p, a.out.as_deref(), &cfg, fmt, &hooks) {
            // 整篇失败也不退出: 记下来继续下一个文件
            Err(e) => {
                println!("  ✗ 转换失败 {}: {e:#}", p.display());
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
        println!("\n--- 错误汇总 ---");
        for p in &failed {
            println!("  整篇失败: {}", p.display());
        }
        for (p, e) in &page_errs {
            let f = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            println!("  {f} 第 {} 页 {}: {}", e.page, e.stage, e.msg);
        }
        std::process::exit(1);
    }
}
