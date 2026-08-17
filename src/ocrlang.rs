//! 识别语言包
//!
//! 一个语言包就是一个 rec 模型文件, 没有别的。识别认得出哪些字完全由 rec 那个
//! `.onnx` 决定 —— 字表是转 ONNX 时写进模型 metadata 的, 换个文件就换了一整套
//! 字。检测(框在哪)和方向分类跟语言无关, 它们只看笔画怎么分布, 不认字。
//!
//! 内置那个 v6 的字表实测覆盖简繁汉字、日文假名、拉丁字母(含 ä ö ü ß ñ á é
//! 这些重音)和希腊字母; **一个字都不覆盖**谚文、西里尔、阿拉伯、泰文、天城文。
//! 所以德语西班牙语法语用户不用下任何东西, 韩语用户不下就什么都认不出来。
//!
//! 一次只能装一套字进去 —— CTC 识别头就一个输出维度, 一个模型一张字表。选了
//! 韩语, 同一页上的汉字就全丢, 这不是取舍是架构。

use anyhow::{anyhow, Result};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::i18n::{self, K};
use crate::tr;

/// 语言包从哪儿下, 按顺序试, 第一个下成的算数
///
/// 地址钉死在 RapidOCR 的 `v3.9.2` 这个 tag 上, 不用 master —— 上游哪天换了
/// 模型, 我们这边的 sha256 会全部对不上, 而那时候用户看到的只是"下载失败",
/// 查起来很远。钉住之后上游怎么动都跟已经发出去的版本无关。
///
/// **眼下只有一个源。** 找过 HuggingFace: 官方的 `PaddlePaddle/*_PP-OCRv5_
/// mobile_rec` 放的是 Paddle 格式(`.pdiparams`), 不是 ONNX; `SWHL/RapidOCR`
/// 只到 v4 而且只有中英两个模型; HF 上没有 RapidAI 的镜像; RapidOCR 的 GitHub
/// Release 一个附件都没挂。带内嵌字表的这批 ONNX 目前只有 ModelScope 一家在
/// 发。ModelScope 的 CDN 直链倒是能拿到, 但那个地址带 `auth_key` 会过期,
/// 写死没用。
///
/// 做成一张表而不是一个常量: 哪天我们自己往别处传一份, 加一行就完事。
const MIRRORS: &[&str] =
    &["https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.2/onnx/"];

/// 一种识别语言
pub struct Pack {
    /// 存进配置、也用在 `--ocr-lang` 上的那个值; 空串 = 内置
    pub code: &'static str,
    /// rec 模型文件名
    pub file: &'static str,
    /// 在 ModelScope 仓库里的路径; 空 = 内置在程序里, 不用下
    pub remote: &'static str,
    pub bytes: u64,
    pub sha: &'static str,
    /// 界面上的名字和那行小字
    pub name: K,
    pub note: K,
}

impl Pack {
    pub fn builtin(&self) -> bool {
        self.remote.is_empty()
    }

    pub fn urls(&self) -> Vec<String> {
        MIRRORS
            .iter()
            .map(|m| format!("{m}{}", self.remote))
            .collect()
    }

    /// "13.5 MB"
    pub fn size(&self) -> String {
        format!("{:.1} MB", self.bytes as f64 / 1_000_000.0)
    }

    /// 下好了没 —— 字节数对得上才算, 只看在不在会把下到一半的残骸当成好的
    pub fn installed(&self) -> bool {
        if self.builtin() {
            return true;
        }
        match dir().map(|d| d.join(self.file)) {
            Some(p) => std::fs::metadata(&p).map(|m| m.len()).ok() == Some(self.bytes),
            None => false,
        }
    }
}

/// 内置的那套 + 能下的那几个
///
/// 实测(同一张韩文样张): 内置三行只吐出一个 `02`, 韩语包三行全对置信度
/// 0.98~1.00; 反过来拿韩语包认中文样张, 三行汉字全丢只剩两个数字。
///
/// 日语那个排在最后, 它跟上面五个不是一回事: 内置本来就认假名和汉字, 拿一张
/// 清晰的日文样张实测两边都是三行全对, 内置给的还是半角的 3, 日语包给的是
/// 全角 ３ —— 落进 docx 反倒是前者更顺手。留着是因为拍糊的、字体怪的日文件
/// 上专门训过的那个也许还有用, 但那个"也许"没量到。
pub const PACKS: &[Pack] = &[
    Pack {
        code: "",
        file: "PP-OCRv6_rec_small.onnx",
        remote: "",
        bytes: 0,
        sha: "",
        name: K::OcrBuiltin,
        note: K::OcrBuiltinNote,
    },
    Pack {
        code: "ko",
        file: "korean_PP-OCRv5_rec_mobile.onnx",
        remote: "PP-OCRv5/rec/korean_PP-OCRv5_rec_mobile.onnx",
        bytes: 13_488_748,
        sha: "cd6e2ea50f6943ca7271eb8c56a877a5a90720b7047fe9c41a2e541a25773c9b",
        name: K::OcrKo,
        note: K::OcrKoNote,
    },
    Pack {
        code: "ru",
        file: "cyrillic_PP-OCRv5_rec_mobile.onnx",
        remote: "PP-OCRv5/rec/cyrillic_PP-OCRv5_rec_mobile.onnx",
        bytes: 8_074_092,
        sha: "90f761b4bfcce0c8c561c0cb5c887b0971d3ec01c32164bdf7374a35b0982711",
        name: K::OcrRu,
        note: K::OcrRuNote,
    },
    Pack {
        code: "ar",
        file: "arabic_PP-OCRv5_rec_mobile.onnx",
        remote: "PP-OCRv5/rec/arabic_PP-OCRv5_rec_mobile.onnx",
        bytes: 8_023_828,
        sha: "c1192e632d0baa9146ae5b756a0e635e3dc63c1733737ebfd1629e87144e9295",
        name: K::OcrAr,
        note: K::OcrArNote,
    },
    Pack {
        code: "th",
        file: "th_PP-OCRv5_rec_mobile.onnx",
        remote: "PP-OCRv5/rec/th_PP-OCRv5_rec_mobile.onnx",
        bytes: 7_915_294,
        sha: "de541dd83161c241ff426f7ecfd602a0ba77d686cf3ab9a6c255ea82fd08006e",
        name: K::OcrTh,
        note: K::OcrThNote,
    },
    Pack {
        code: "hi",
        file: "devanagari_PP-OCRv5_rec_mobile.onnx",
        remote: "PP-OCRv5/rec/devanagari_PP-OCRv5_rec_mobile.onnx",
        bytes: 7_940_361,
        sha: "d6f0a906580e3fa6b324a318718f1f31f268b6ea8ef985f91c2012a37f52c91e",
        name: K::OcrHi,
        note: K::OcrHiNote,
    },
    Pack {
        // 日语只有 v4, 上游没出 v5 的。v4 的输入高度跟我们写死的 48 对得上,
        // v3 那批是 32, 喂 48 进去直接报维度不符, 所以 v3 一个都不能收
        code: "ja",
        file: "japan_PP-OCRv4_rec_mobile.onnx",
        remote: "PP-OCRv4/rec/japan_PP-OCRv4_rec_mobile.onnx",
        bytes: 9_753_335,
        sha: "e1075a67dba758ecfc7ebc78a10ae61c95ac8fb66a9c86fab5541e33f085cb7a",
        name: K::OcrJa,
        note: K::OcrJaNote,
    },
];

pub fn by_code(code: &str) -> &'static Pack {
    PACKS.iter().find(|p| p.code == code).unwrap_or(&PACKS[0])
}

/// 能给 `--ocr-lang` 的那几个值, 拼成一行给报错用
///
/// 不列内置那个: 它的 code 是空串, 印出来是个洞。不给 `--ocr-lang` 就是它。
pub fn codes() -> String {
    PACKS
        .iter()
        .filter(|p| !p.builtin())
        .map(|p| p.code)
        .collect::<Vec<_>>()
        .join(" / ")
}

/// 语言包存哪儿
///
/// 不跟内置那三个模型放一起: 那三个在程序旁边(macOS 上还在 .app 里面), 装到
/// /Applications 之后普通用户根本写不进去。这儿用的是用户缓存目录, 一定可写,
/// 而且用户自己清缓存时它跟着走 —— 里面全是能重新下回来的东西。
///
/// 只算一次: `installed()` 会被界面每帧问一遍, 每帧都 create_dir_all 一次
/// 是白扔的系统调用。
pub fn dir() -> Option<PathBuf> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = crate::cache_dir()?.join("scannedpdf2doc/packs");
        std::fs::create_dir_all(&d).ok()?;
        Some(d)
    })
    .clone()
}

/// 装好的语言包放在哪个目录里(交给 `EngineOptions.rec_dir`)
pub fn installed_dir(p: &Pack) -> Option<PathBuf> {
    (!p.builtin()).then(dir).flatten()
}

/// 下一个语言包, 边下边报进度(0~100)
///
/// 挨个源试, 第一个下成的算数; 全都不成就把最后一次的错抛出去 —— 抛第一次的
/// 没有意义, 用户真正卡在哪一步看的是最后那次。
///
/// 先下到 `.part` 再改名: 下到一半被关掉的话, 留在磁盘上的是个 .part 而不是
/// 一个大小对不上的模型文件。改名在同一个目录里, 是原子的。
pub fn download(pack: &Pack, on_progress: &dyn Fn(u32)) -> Result<PathBuf> {
    let dir = dir().ok_or_else(|| anyhow!("{}", tr!(K::PackNoDir)))?;
    let dst = dir.join(pack.file);
    let part = dir.join(format!("{}.part", pack.file));

    let urls = pack.urls();
    let mut last: Option<anyhow::Error> = None;
    for url in &urls {
        match fetch(url, pack, &part, on_progress) {
            Ok(()) => {
                std::fs::rename(&part, &dst)?;
                return Ok(dst);
            }
            Err(e) => {
                // 换下一个源之前先把残骸清掉
                let _ = std::fs::remove_file(&part);
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("没有可用的下载源")))
}

/// 从一个具体地址下到 `part`, 校验字节数和 sha256; 不改名
///
/// 走 curl 而不是链一个 HTTP 客户端 crate。理由跟 `gui.rs` 里不用 `open` 那个
/// crate 一样, 但这里更硬: 纯 Rust 的 TLS 栈要拖进 rustls + ring 一整条链,
/// 而 ring 里那段汇编挂的是 OpenSSL 那套授权 —— 这个项目卖点之一就是"闭源
/// 商用无需授权", 为一年用不到几次的下载功能新增一条要交代的授权不划算。
/// curl 在 macOS、Windows 10 1803+ 和几乎所有桌面 Linux 上都是现成的; 没有
/// 的话就明说没有, 用户仍可以自己把 .onnx 拷进那个目录。
///
/// 进度靠盯 `.part` 的大小, 不解析 curl 的输出 —— 那个格式没有任何稳定性承诺。
fn fetch(url: &str, pack: &Pack, part: &Path, on_progress: &dyn Fn(u32)) -> Result<()> {
    let _ = std::fs::remove_file(part);
    let mut child = std::process::Command::new("curl")
        // -f: HTTP 错误码也当失败(默认 curl 会把 404 页面当正文存下来)
        // -L: ModelScope 那个地址是 302 跳到 CDN 的
        .args(["-fsSL", "--connect-timeout", "20", "-o"])
        .arg(part)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("{}: {e}", tr!(K::PackNoCurl)))?;

    let mut last = u32::MAX;
    loop {
        if let Some(st) = child.try_wait()? {
            if !st.success() {
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                let code = st.code().unwrap_or(-1);
                let err = err.trim();
                return Err(if err.is_empty() {
                    anyhow!("{}", tr!(K::PackHttpFailed, code))
                } else {
                    anyhow!("{} — {err}", tr!(K::PackHttpFailed, code))
                });
            }
            break;
        }
        let got = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
        let pct = ((got * 100 / pack.bytes.max(1)) as u32).min(100);
        if pct != last {
            last = pct;
            on_progress(pct);
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
    }

    let n = std::fs::metadata(part)?.len();
    if n != pack.bytes {
        return Err(anyhow!("{}", tr!(K::PackShort, n, pack.bytes)));
    }
    // 校验哈希。HTTPS 加上字节数对得上已经挡掉了绝大多数情况; 留这一道是因为
    // 剩下那种最难查 —— 一个内容不对的模型不会报错, 它照样跑, 只是认出来的每
    // 个字都是错的。多源之后更要紧: 镜像未必跟主源同步。
    let h = sha256_of(part)?;
    if h != pack.sha {
        return Err(anyhow!("{}", tr!(K::PackBadHash, h)));
    }
    on_progress(100);
    Ok(())
}

/// 分块算, 不把十几兆整个读进内存
fn sha256_of(p: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// 备好这次识别要用的语言包: 不在就现下
///
/// 缺了就现下, 而不是退回内置。退回内置在这里是最坏的一种处理: 韩文页用中文
/// 模型认一遍, 出来的不是"没结果", 是一整页**看着像结果的错字** —— 用户拿到
/// 一份 Word 才发现不对, 而那时候已经没有任何线索指向"语言包没装上"。
pub fn prepare(pack: &Pack, on_progress: &dyn Fn(u32)) -> Result<()> {
    if pack.builtin() || pack.installed() {
        return Ok(());
    }
    download(pack, on_progress).map(|_| ())
}

/// 把一个包的名字翻成当前界面语言
pub fn name(p: &Pack) -> &'static str {
    i18n::t(p.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 表里每一行都得能拼出地址、code 不许重名 —— 重名的话 by_code 会静悄悄
    /// 地拿到前一个, 用户选了日语却在用韩语模型
    #[test]
    fn pack_table_is_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for p in PACKS {
            assert!(seen.insert(p.code), "code 重了: {}", p.code);
            assert!(!p.file.is_empty());
            if p.builtin() {
                continue;
            }
            assert!(p.bytes > 0, "{} 没写字节数", p.code);
            assert_eq!(p.sha.len(), 64, "{} 的 sha256 长度不对", p.code);
            assert!(
                p.remote.ends_with(p.file),
                "{} 的远端路径对不上文件名",
                p.code
            );
            assert_eq!(p.urls().len(), MIRRORS.len());
        }
        assert!(PACKS[0].builtin(), "内置那个要排在最前");
    }
}
