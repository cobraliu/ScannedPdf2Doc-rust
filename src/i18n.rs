//! 界面语言
//!
//! 七种语言, 跟 iOS 那个 App 对齐(简体、繁体、英、西、德、日、韩)。译文直接
//! 写在这张表里, 不外挂 .po / .json:
//!
//! - 少一份运行时要找的文件, 也就少一类"装到别的机器上就成了英文"的故障;
//! - 缺译在编译期就报出来 —— 宏要求每个 key 七种都给全, 漏一个编不过。
//!
//! 当前语言存成一个全局。看着不雅, 但这是个桌面程序: 整个进程只有一个界面,
//! 一个界面只有一种语言。做成参数的话, 从 GUI 一路传到 `lib.rs` 里那些
//! `hooks.say(...)`, 要给十几个函数加一个谁都不关心的形参。

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    /// 简体中文
    Zh,
    /// 繁體中文
    Hant,
    En,
    Es,
    De,
    Ja,
    Ko,
}

impl Lang {
    pub const ALL: [Lang; 7] = [
        Lang::Zh,
        Lang::Hant,
        Lang::En,
        Lang::Es,
        Lang::De,
        Lang::Ja,
        Lang::Ko,
    ];

    /// 存进配置文件、也用在 `--lang` 上的那个值
    pub fn tag(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::Hant => "zh-Hant",
            Lang::En => "en",
            Lang::Es => "es",
            Lang::De => "de",
            Lang::Ja => "ja",
            Lang::Ko => "ko",
        }
    }

    /// 菜单里怎么写
    ///
    /// 一律用母语名, 不跟着当前界面语言变: 界面正显示着看不懂的语言时, 唯一
    /// 还认得出的就是自己那门语言写的名字 —— 「Deutsch」谁都找得到,
    /// 「德语」对德国人毫无用处。
    pub fn label(self) -> &'static str {
        match self {
            Lang::Zh => "简体中文",
            Lang::Hant => "繁體中文",
            Lang::En => "English",
            Lang::Es => "Español",
            Lang::De => "Deutsch",
            Lang::Ja => "日本語",
            Lang::Ko => "한국어",
        }
    }

    /// 认一个 BCP-47 标签或者 POSIX locale: `zh`、`zh-Hant`、`zh_TW.UTF-8` 都收
    pub fn parse(s: &str) -> Option<Lang> {
        // zh_CN.UTF-8 -> zh_CN -> zh-cn
        let s = s.split('.').next().unwrap_or(s).replace('_', "-");
        let s = s.to_ascii_lowercase();
        let mut it = s.split('-');
        let base = it.next()?;
        let rest: Vec<&str> = it.collect();
        Some(match base {
            // 繁体只看地区和 script: 大陆和新加坡是简体, 台港澳是繁体
            "zh" => {
                if rest
                    .iter()
                    .any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo"))
                {
                    Lang::Hant
                } else {
                    Lang::Zh
                }
            }
            "en" => Lang::En,
            "es" => Lang::Es,
            "de" => Lang::De,
            "ja" => Lang::Ja,
            "ko" => Lang::Ko,
            _ => return None,
        })
    }
}

/// 当前语言, 存的是 `Lang` 的判别值
static CUR: AtomicU8 = AtomicU8::new(0);

pub fn set(l: Lang) {
    CUR.store(l as u8, Ordering::Relaxed);
}

pub fn cur() -> Lang {
    Lang::ALL
        .get(CUR.load(Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or(Lang::Zh)
}

/// 系统语言
///
/// 认不出来的语种回落到英文而不是中文: 一台系统语言是法语的电脑, 英文界面
/// 至少还能读。整个探测都失败(拿不到任何 locale)才回落到中文 —— 那是这个
/// 程序原来唯一的语言, 保持老用户升级上来看到的东西不变。
pub fn detect() -> Lang {
    match sys_locale() {
        Some(s) => Lang::parse(&s).unwrap_or(Lang::En),
        None => Lang::Zh,
    }
}

fn sys_locale() -> Option<String> {
    // Linux 和从终端启动的 macOS 都能从这儿拿到。"C" / "POSIX" 是"没设置"的
    // 意思, 不是一种语言
    for k in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
        if let Ok(v) = std::env::var(k) {
            let v = v.split(':').next().unwrap_or(&v).trim().to_string();
            if !v.is_empty() && v != "C" && v != "POSIX" {
                return Some(v);
            }
        }
    }
    sys_locale_native()
}

/// 从 Finder 双击起来的 .app 拿不到任何 LANG, 只能问系统
///
/// 走 `defaults` 而不是链 CoreFoundation: 一次进程调用几十毫秒, 只在启动时
/// 发生一次, 换来的是不用为了读一个字符串给这个程序加一条系统框架依赖。
#[cfg(target_os = "macos")]
fn sys_locale_native() -> Option<String> {
    let out = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLocale"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Windows 上环境变量基本都是空的, 直接问 Win32
///
/// `GetUserDefaultLocaleName` 从 Vista 起就在 kernel32 里, 声明四行就能用,
/// 不值得为它引一条 windows-sys 依赖。
#[cfg(windows)]
fn sys_locale_native() -> Option<String> {
    // LOCALE_NAME_MAX_LENGTH = 85
    let mut buf = [0u16; 85];
    unsafe extern "system" {
        fn GetUserDefaultLocaleName(name: *mut u16, len: i32) -> i32;
    }
    let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
    // 返回值含结尾的 \0
    if n <= 1 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..(n as usize - 1)]))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn sys_locale_native() -> Option<String> {
    None
}

/// 把 `{0}` `{1}` 换成实参
///
/// 用编号而不是顺序占位, 是因为各语言的语序不一样 —— 德语的"第 3 页失败"里
/// 页码和动词的位置跟中文对不上, 编号让译者能自由调换。
pub fn fill(tpl: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(tpl.len() + 16);
    let b = tpl.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && j < b.len() && b[j] == b'}' {
                let n: usize = tpl[i + 1..j].parse().unwrap_or(usize::MAX);
                out.push_str(args.get(n).copied().unwrap_or(""));
                i = j + 1;
                continue;
            }
        }
        // 一个字符可能占好几个字节, 只能整段抄过去
        let c = tpl[i..].chars().next().expect("i 停在字符边界上");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

pub fn t(k: K) -> &'static str {
    k.raw(cur())
}

/// 取一条译文并填参: `tr!(K::Added, n)`
#[macro_export]
macro_rules! tr {
    ($k:expr) => { $crate::i18n::t($k) };
    // 先 to_string 再借成 &str: 临时的 String 活到整条语句结束, 也就活过
    // fill 这次调用
    ($k:expr, $($a:expr),+ $(,)?) => {
        $crate::i18n::fill($crate::i18n::t($k), &[$($a.to_string().as_str()),+])
    };
}

macro_rules! strings {
    ($(
        $k:ident
            zh $zh:literal
            ht $ht:literal
            en $en:literal
            es $es:literal
            de $de:literal
            ja $ja:literal
            ko $ko:literal;
    )+) => {
        /// 每一条要翻的话
        #[derive(Clone, Copy, Debug)]
        pub enum K { $($k,)+ }

        impl K {
            fn raw(self, l: Lang) -> &'static str {
                match self {
                    $(K::$k => match l {
                        Lang::Zh => $zh,
                        Lang::Hant => $ht,
                        Lang::En => $en,
                        Lang::Es => $es,
                        Lang::De => $de,
                        Lang::Ja => $ja,
                        Lang::Ko => $ko,
                    },)+
                }
            }
        }
    };
}

// ─────────────────────────── 译文表 ───────────────────────────
//
// 「Word」「Excel」「PDF」不翻 —— 各语言界面上都写这几个词。
strings! {

WindowTitle
    zh "扫描件 PDF → Word / Excel"
    ht "掃描件 PDF → Word / Excel"
    en "Scanned PDF → Word / Excel"
    es "PDF escaneado → Word / Excel"
    de "Gescanntes PDF → Word / Excel"
    ja "スキャン PDF → Word / Excel"
    ko "스캔 PDF → Word / Excel";

// ── 顶栏 ──

AddPdf
    zh "添加 PDF…"
    ht "新增 PDF…"
    en "Add PDF…"
    es "Añadir PDF…"
    de "PDF hinzufügen…"
    ja "PDF を追加…"
    ko "PDF 추가…";

Clear
    zh "清空"
    ht "清空"
    en "Clear"
    es "Vaciar"
    de "Leeren"
    ja "クリア"
    ko "비우기";

DefaultOut
    zh "默认输出:"
    ht "預設輸出："
    en "Output:"
    es "Salida:"
    de "Ausgabe:"
    ja "出力:"
    ko "출력:";

DefaultOutTip
    zh "队列里可以给单个文件另设格式, 没设的都跟这里走"
    ht "佇列裡可以給個別檔案另設格式，沒設的都跟這裡走"
    en "Each file in the queue can override this; the rest follow it"
    es "Cada archivo de la cola puede cambiarlo; el resto sigue esto"
    de "Jede Datei in der Warteschlange kann davon abweichen, der Rest folgt"
    ja "キューの各ファイルで個別に変えられます。指定がなければこれに従います"
    ko "대기열의 파일마다 따로 정할 수 있고, 정하지 않으면 이걸 따릅니다";

FmtBoth
    zh "两份都要"
    ht "兩份都要"
    en "Both"
    es "Ambos"
    de "Beide"
    ja "両方"
    ko "둘 다";

FmtBothShort
    zh "两份"
    ht "兩份"
    en "Both"
    es "Ambos"
    de "Beide"
    ja "両方"
    ko "둘 다";

FmtDefault
    zh "默认·{0}"
    ht "預設·{0}"
    en "Default · {0}"
    es "Predet. · {0}"
    de "Standard · {0}"
    ja "既定 · {0}"
    ko "기본 · {0}";

OutDirPick
    zh "输出目录…"
    ht "輸出目錄…"
    en "Output folder…"
    es "Carpeta de salida…"
    de "Zielordner…"
    ja "出力フォルダ…"
    ko "출력 폴더…";

OutDirChange
    zh "改输出目录"
    ht "改輸出目錄"
    en "Change folder"
    es "Cambiar carpeta"
    de "Ordner ändern"
    ja "フォルダを変更"
    ko "폴더 바꾸기";

OutDirReset
    zh "改回源文件同目录"
    ht "改回原始檔案所在目錄"
    en "Back to the source file's folder"
    es "Volver a la carpeta del archivo original"
    de "Zurück zum Ordner der Quelldatei"
    ja "元のファイルと同じフォルダに戻す"
    ko "원본 파일이 있는 폴더로 되돌리기";

OpenResult
    zh "打开结果"
    ht "開啟結果"
    en "Show results"
    es "Ver resultados"
    de "Ergebnisse zeigen"
    ja "結果を表示"
    ko "결과 열기";

Stop
    zh "停止"
    ht "停止"
    en "Stop"
    es "Detener"
    de "Anhalten"
    ja "停止"
    ko "중지";

Stopping
    zh "正在停止…"
    ht "正在停止…"
    en "Stopping…"
    es "Deteniendo…"
    de "Wird angehalten…"
    ja "停止中…"
    ko "중지하는 중…";

StopTip
    zh "Esc"
    ht "Esc"
    en "Esc"
    es "Esc"
    de "Esc"
    ja "Esc"
    ko "Esc";

Start
    zh "开始转换"
    ht "開始轉換"
    en "Convert"
    es "Convertir"
    de "Umwandeln"
    ja "変換開始"
    ko "변환 시작";

StartTip
    zh "回车"
    ht "Enter"
    en "Enter"
    es "Intro"
    de "Eingabetaste"
    ja "Enter"
    ko "Enter";

Advanced
    zh "高级参数"
    ht "進階參數"
    en "Advanced"
    es "Avanzado"
    de "Erweitert"
    ja "詳細設定"
    ko "고급 설정";

OutTo
    zh "输出到 {0}"
    ht "輸出到 {0}"
    en "Saving to {0}"
    es "Guardando en {0}"
    de "Speichern nach {0}"
    ja "{0} に出力"
    ko "{0} 에 저장";

OutToSrc
    zh "输出到源文件所在目录"
    ht "輸出到原始檔案所在目錄"
    en "Saving next to each source file"
    es "Guardando junto a cada archivo original"
    de "Speichern neben der jeweiligen Quelldatei"
    ja "元のファイルと同じ場所に出力"
    ko "원본 파일과 같은 폴더에 저장";

Ready
    zh "就绪"
    ht "就緒"
    en "Ready"
    es "Listo"
    de "Bereit"
    ja "準備完了"
    ko "준비됨";

Undo
    zh "撤销"
    ht "復原"
    en "Undo"
    es "Deshacer"
    de "Rückgängig"
    ja "元に戻す"
    ko "되돌리기";

UndoTip
    zh "把刚移出队列的文件放回去"
    ht "把剛移出佇列的檔案放回去"
    en "Put the files just removed back in the queue"
    es "Devolver a la cola los archivos recién quitados"
    de "Die eben entfernten Dateien zurück in die Liste holen"
    ja "いま外したファイルを列に戻す"
    ko "방금 뺀 파일을 목록에 되돌립니다";

Removed
    zh "移出 {0} 项(可撤销)"
    ht "移出 {0} 項(可復原)"
    en "Removed {0} (can be undone)"
    es "Se quitaron {0} (se puede deshacer)"
    de "{0} entfernt (rückgängig möglich)"
    ja "{0} 件を外しました(元に戻せます)"
    ko "{0}개를 뺐습니다(되돌릴 수 있음)";

RetryTip
    zh "只重转这一个"
    ht "只重轉這一個"
    en "Convert just this one again"
    es "Volver a convertir solo este"
    de "Nur diese eine erneut umwandeln"
    ja "これだけ変換し直す"
    ko "이것만 다시 변환";

RemoveTip
    zh "移出队列"
    ht "移出佇列"
    en "Remove from the queue"
    es "Quitar de la cola"
    de "Aus der Liste nehmen"
    ja "列から外す"
    ko "목록에서 빼기";

// ── 高级参数 ──

LongEdge
    zh "渲染长边像素"
    ht "算圖長邊像素"
    en "Render long edge (px)"
    es "Lado largo del render (px)"
    de "Lange Kante beim Rendern (px)"
    ja "レンダリング長辺 (px)"
    ko "렌더링 긴 변 (px)";

LongEdgeTip
    zh "实际 dpi 由页面尺寸倒推并夹在 150~300; 调小识别快但小字容易丢"
    ht "實際 dpi 由頁面尺寸回推並夾在 150~300；調小辨識快但小字容易漏"
    en "The real dpi is derived from the page size and clamped to 150–300. Lower is faster but drops small text"
    es "El dpi real se deduce del tamaño de página y se limita a 150–300. Más bajo es más rápido, pero pierde letra pequeña"
    de "Die tatsächliche dpi ergibt sich aus der Seitengröße und liegt zwischen 150 und 300. Niedriger ist schneller, verliert aber kleine Schrift"
    ja "実際の dpi はページ寸法から逆算し 150〜300 に収めます。小さくすると速い代わりに小さな文字が落ちます"
    ko "실제 dpi는 페이지 크기에서 역산해 150~300 사이로 맞춥니다. 낮추면 빠르지만 작은 글자를 놓칩니다";

HelpNoFigures
    zh "不要保留印章/签名/插图"
    ht "不要保留印章/簽名/插圖"
    en "Do not keep stamps, signatures or figures"
    es "No conservar sellos, firmas ni figuras"
    de "Stempel, Unterschriften und Abbildungen nicht übernehmen"
    ja "印章・署名・図を残さない"
    ko "도장·서명·그림을 남기지 않습니다";

Figures
    zh "保留印章/签名/插图"
    ht "保留印章/簽名/插圖"
    en "Keep stamps, signatures and figures"
    es "Conservar sellos, firmas y figuras"
    de "Stempel, Unterschriften und Abbildungen übernehmen"
    ja "印章・署名・図を残す"
    ko "도장·서명·그림 남기기";

FiguresTip
    zh "识别只产出「哪儿有什么字」, 一页上凡是不是字的东西到这一步就没了 —— 合同末尾那个红章、手签的名字、示意图和 logo, 转出来的 Word 里一概不见。丢的偏偏是最要紧的几样。\n\n找法是减法: 页面上的墨迹, 减去识别认下的字, 减去表格框线, 剩下成团的就是它。裁的是彩色页图, 红章不会变成一团灰印子。\n\n章盖在字上不要紧: 挖掉的是字框, 章的圈还在。"
    ht "識別只產出「哪兒有什麼字」，一頁上凡是不是字的東西到這一步就沒了 —— 合同末尾那個紅章、手簽的名字、示意圖和 logo，轉出來的 Word 裡一概不見。丟的偏偏是最要緊的幾樣。\n\n找法是減法：頁面上的墨跡，減去識別認下的字，減去表格框線，剩下成團的就是它。裁的是彩色頁圖，紅章不會變成一團灰印子。\n\n章蓋在字上不要緊：挖掉的是字框，章的圈還在。"
    en "Recognition only produces \"what text is where\". Everything on the page that is not text ends here — the red seal at the foot of a contract, a handwritten name, a diagram, a logo: none of it reaches the Word file. Those tend to be exactly the parts that matter.\n\nThey are found by subtraction: take the ink on the page, remove the text that was recognised, remove the table rules, and what is left in clumps is the figure. The crop is taken from a colour rendering, so a red seal does not come out as a grey smudge.\n\nA seal stamped over text is fine: what gets removed is the text box, and the ring of the seal stays."
    es "El reconocimiento solo produce «qué texto hay y dónde». Todo lo que en la página no es texto se pierde aquí: el sello rojo al pie de un contrato, una firma manuscrita, un esquema, un logotipo. Nada de eso llega al archivo de Word, y suele ser justo lo que importa.\n\nSe encuentran por resta: se toma la tinta de la página, se quita el texto reconocido, se quitan las líneas de las tablas, y lo que queda agrupado es la figura. El recorte se hace sobre una versión en color, así que un sello rojo no sale como un borrón gris.\n\nQue el sello caiga encima del texto no importa: lo que se quita es el recuadro del texto y el anillo del sello permanece."
    de "Die Erkennung liefert nur „welcher Text steht wo“. Alles auf der Seite, was kein Text ist, endet hier — der rote Stempel unter einem Vertrag, ein handgeschriebener Name, eine Skizze, ein Logo: nichts davon erreicht die Word-Datei. Ausgerechnet das sind oft die wichtigsten Teile.\n\nGefunden werden sie durch Subtraktion: die Tinte auf der Seite, abzüglich des erkannten Textes, abzüglich der Tabellenlinien — was in Klumpen übrig bleibt, ist die Abbildung. Ausgeschnitten wird aus einer farbigen Fassung, ein roter Stempel wird also kein grauer Fleck.\n\nEin Stempel über dem Text ist kein Problem: entfernt wird der Textrahmen, der Ring des Stempels bleibt."
    ja "認識が出すのは「どこに何の文字があるか」だけです。ページ上の文字でないものはここで消えます —— 契約書末尾の朱印、手書きの署名、図、ロゴ。どれも Word には届きません。よりによって一番大事なものばかりです。\n\n探し方は引き算です。ページの墨から、認識できた文字を引き、罫線を引く。かたまりで残ったものが図です。切り出しはカラーの描画から行うので、朱印が灰色のしみになりません。\n\n印が文字に重なっていても平気です。取り除くのは文字の枠で、印の輪は残ります。"
    ko "인식이 내놓는 것은 \"어디에 어떤 글자가 있는지\"뿐입니다. 페이지에서 글자가 아닌 것은 여기서 사라집니다 — 계약서 끝의 붉은 도장, 손으로 쓴 이름, 도해, 로고. 어느 것도 Word 파일에 닿지 않습니다. 하필 가장 중요한 것들입니다.\n\n찾는 방법은 뺄셈입니다. 페이지의 잉크에서 인식된 글자를 빼고 표의 괘선을 빼면, 덩어리로 남는 것이 그림입니다. 잘라내기는 컬러 렌더링에서 하므로 붉은 도장이 회색 얼룩이 되지 않습니다.\n\n도장이 글자 위에 찍혀 있어도 괜찮습니다. 지우는 것은 글자 상자이고 도장의 테두리는 남습니다.";

HelpNoFlatten
    zh "不要摊平不匀的光照"
    ht "不要攤平不勻的光照"
    en "Do not even out uneven lighting"
    es "No igualar la iluminación desigual"
    de "Ungleichmäßige Beleuchtung nicht ausgleichen"
    ja "むらのある明るさを平らにしない"
    ko "고르지 않은 밝기를 평탄화하지 않습니다";

Flatten
    zh "把明暗不匀摊平"
    ht "把明暗不勻攤平"
    en "Even out uneven lighting"
    es "Igualar la iluminación desigual"
    de "Ungleichmäßige Beleuchtung ausgleichen"
    ja "明るさのむらを平らに"
    ko "고르지 않은 밝기 평탄화";

FlattenTip
    zh "手机拍的纸多半一边亮一边暗。暗的那侧白纸自己就掉到了「算墨迹」的阈值以下, 找框线的一上来就被喂了一大片黑, 表格全乱。\n\n做法是按格子估出「这块地方的纸有多白」, 再照它把每个像素归一化。整格被印章盖死时靠旁边的纸补回来, 印章不会被除成白板。\n\n扫描仪出来的页面本来就是平的, 量到平就原样放过, 不花这个时间。"
    ht "手機拍的紙多半一邊亮一邊暗。暗的那側白紙自己就掉到了「算墨跡」的閾值以下，找框線的一上來就被餵了一大片黑，表格全亂。\n\n做法是按格子估出「這塊地方的紙有多白」，再照它把每個像素歸一化。整格被印章蓋死時靠旁邊的紙補回來，印章不會被除成白板。\n\n掃描儀出來的頁面本來就是平的，量到平就原樣放過，不花這個時間。"
    en "A page photographed with a phone is usually brighter on one side. On the dark side the blank paper itself falls below the \"this is ink\" threshold, so rule detection is handed a large black slab and the table falls apart.\n\nThe fix estimates how white the paper is in each patch of the page and normalises every pixel against that. Where a stamp covers a whole patch, the surrounding paper fills it back in, so the stamp is not bleached away.\n\nPages from a scanner are already even; when the page measures flat it is passed through untouched and costs nothing."
    es "Una página fotografiada con el móvil suele quedar más clara de un lado. En el lado oscuro el papel en blanco cae por debajo del umbral de «esto es tinta», así que la detección de líneas recibe un gran bloque negro y la tabla se deshace.\n\nLa corrección estima lo blanco que es el papel en cada zona de la página y normaliza cada píxel según eso. Donde un sello cubre una zona entera, el papel de alrededor la rellena, así que el sello no se blanquea.\n\nLas páginas de un escáner ya son uniformes; si la página se mide plana, pasa intacta y no cuesta nada."
    de "Eine mit dem Handy fotografierte Seite ist meist auf einer Seite heller. Auf der dunklen Seite rutscht schon das leere Papier unter die Schwelle „das ist Tinte“, die Linienerkennung bekommt einen großen schwarzen Block vorgesetzt und die Tabelle zerfällt.\n\nDie Korrektur schätzt feldweise, wie weiß das Papier dort ist, und normiert jedes Pixel darauf. Wo ein Stempel ein ganzes Feld bedeckt, füllt das umliegende Papier es wieder auf, der Stempel wird also nicht weggebleicht.\n\nSeiten aus dem Scanner sind ohnehin gleichmäßig; misst sich die Seite als flach, geht sie unverändert durch und kostet nichts."
    ja "スマートフォンで撮った紙は片側だけ明るいのが普通です。暗い側では白紙そのものが「これはインク」のしきい値を下回り、罫線検出はいきなり大きな黒い塊を渡されて表が崩れます。\n\nページを升目に分けて「ここの紙はどれくらい白いか」を見積もり、それに合わせて各画素を正規化します。升目が印章で埋まっている場合は周りの紙から補うので、印章が白く飛ぶことはありません。\n\nスキャナーのページはもともと均一で、平らだと測れたらそのまま通すので時間はかかりません。"
    ko "휴대폰으로 찍은 종이는 대개 한쪽이 더 밝습니다. 어두운 쪽에서는 빈 종이 자체가 \"이것은 잉크\" 기준값 아래로 떨어져, 괘선 검출이 커다란 검은 덩어리를 먼저 받아 표가 무너집니다.\n\n페이지를 칸으로 나눠 \"이 자리의 종이가 얼마나 흰지\"를 추정하고 그에 맞춰 각 화소를 정규화합니다. 칸 전체가 도장으로 덮인 곳은 주변 종이로 메우므로 도장이 하얗게 날아가지 않습니다.\n\n스캐너로 만든 페이지는 원래 고르므로, 평탄하다고 측정되면 그대로 통과시켜 시간이 들지 않습니다.";

HelpNoDeskew
    zh "不要自动摆正扫歪的页面"
    ht "不要自動轉正掃歪的頁面"
    en "Do not straighten pages that were scanned at an angle"
    es "No enderezar las páginas escaneadas torcidas"
    de "Schief eingescannte Seiten nicht gerade rücken"
    ja "傾いて取り込まれたページをまっすぐにしない"
    ko "기울어져 스캔된 쪽을 바로잡지 않습니다";

Deskew
    zh "把扫歪的页面转正"
    ht "把掃歪的頁面轉正"
    en "Straighten tilted scans"
    es "Enderezar escaneos torcidos"
    de "Schiefe Scans gerade rücken"
    ja "傾いたスキャンをまっすぐに"
    ko "기울어진 스캔 바로잡기";

DeskewTip
    zh "扫描件和拍照件多半歪着一点。一度看着不多, 但整页宽度上一条横线两端就差了三十来个像素 —— 框线会被判断成断开的好几截, 表格还原不出来。\n\n量出角度再转回去, 每页多花几十毫秒。原生 PDF 不用转, 这个开关对它没影响。"
    ht "掃描件和拍照件多半歪著一點。一度看著不多，但整頁寬度上一條橫線兩端就差了三十來個像素 —— 框線會被判斷成斷開的好幾截，表格還原不出來。\n\n量出角度再轉回去，每頁多花幾十毫秒。原生 PDF 不用轉，這個開關對它沒影響。"
    en "Scans and photos are usually a little tilted. One degree sounds harmless, but across a full page it puts the two ends of a horizontal rule some thirty pixels apart — the rule gets read as several broken pieces and the table falls apart.\n\nMeasuring the angle and turning it back costs a few tens of milliseconds per page. Native PDFs need no straightening, so this switch does nothing for them."
    es "Los escaneos y las fotos suelen quedar algo torcidos. Un grado parece poco, pero a lo ancho de una página deja los dos extremos de una línea horizontal separados unos treinta píxeles: la línea se lee como varios trozos sueltos y la tabla se deshace.\n\nMedir el ángulo y devolverlo cuesta unas decenas de milisegundos por página. Los PDF nativos no necesitan enderezarse, así que esta opción no les afecta."
    de "Scans und Fotos sind meist etwas schief. Ein Grad klingt harmlos, doch über eine ganze Seite liegen die beiden Enden einer waagerechten Linie rund dreißig Pixel auseinander — die Linie wird als mehrere abgerissene Stücke gelesen und die Tabelle zerfällt.\n\nWinkel messen und zurückdrehen kostet einige zehn Millisekunden pro Seite. Native PDFs brauchen das nicht, für sie ändert dieser Schalter nichts."
    ja "スキャンや写真はたいてい少し傾いています。一度と聞くとわずかですが、ページの幅いっぱいでは横線の両端が三十ピクセルほどずれ、線が何本もの切れ端として読まれて表が崩れます。\n\n角度を測って戻すのに 1 ページあたり数十ミリ秒かかります。ネイティブ PDF には不要なので、この切り替えは効きません。"
    ko "스캔본과 사진은 대개 조금 기울어 있습니다. 1도는 대수롭지 않아 보이지만 페이지 폭 전체로는 가로선의 양 끝이 서른 픽셀쯤 어긋나, 선이 끊긴 여러 조각으로 읽히고 표가 무너집니다.\n\n각도를 재서 되돌리는 데 쪽당 수십 밀리초가 듭니다. 네이티브 PDF에는 필요 없어 이 설정은 영향을 주지 않습니다.";

TextLayer
    zh "有文字层就直接用"
    ht "有文字層就直接用"
    en "Use the PDF's text layer"
    es "Usar la capa de texto del PDF"
    de "Textebene des PDFs verwenden"
    ja "PDF の文字レイヤーを使う"
    ko "PDF 텍스트 레이어 사용";

TextLayerTip
    zh "原生 PDF(Word/Excel 直接导出的)自带文字, 直接取比识别又快又准。\n\n扫描件没有文字层, 这个开关对它没影响; 只有当扫描件被别的工具塞过一层不准的文字时, 才需要关掉重认。"
    ht "原生 PDF(Word/Excel 直接匯出的)自帶文字，直接取比辨識又快又準。\n\n掃描件沒有文字層，這個開關對它沒影響；只有當掃描件被別的工具塞過一層不準的文字時，才需要關掉重認。"
    en "PDFs exported straight from Word or Excel carry their own text — taking it is both faster and more accurate than recognising it.\n\nScans have no text layer, so this switch does nothing for them; turn it off only when a scan carries an inaccurate text layer added by some other tool."
    es "Los PDF exportados directamente desde Word o Excel llevan su propio texto: tomarlo es más rápido y más exacto que reconocerlo.\n\nLos escaneos no tienen capa de texto, así que esta opción no les afecta; desactívala solo si un escaneo trae una capa de texto inexacta añadida por otra herramienta."
    de "Direkt aus Word oder Excel exportierte PDFs bringen ihren Text mit — ihn zu übernehmen ist schneller und genauer als ihn zu erkennen.\n\nScans haben keine Textebene, für sie ändert dieser Schalter nichts; abschalten nur, wenn ein Scan eine fehlerhafte, von einem anderen Werkzeug hinzugefügte Textebene mitbringt."
    ja "Word や Excel からそのまま書き出した PDF は文字を持っているので、認識するより速く正確です。\n\nスキャンには文字レイヤーがないためこの切り替えは効きません。他のツールが不正確な文字レイヤーを付けたスキャンのときだけ切ってください。"
    ko "Word나 Excel에서 그대로 내보낸 PDF는 문자를 가지고 있어, 인식하는 것보다 빠르고 정확합니다.\n\n스캔본에는 텍스트 레이어가 없어 이 설정이 영향을 주지 않습니다. 다른 도구가 부정확한 텍스트 레이어를 넣은 스캔본일 때만 꺼 주세요.";

GridTables
    zh "按框线还原表格"
    ht "依框線還原表格"
    en "Rebuild ruled tables"
    es "Reconstruir tablas con líneas"
    de "Tabellen mit Linien rekonstruieren"
    ja "罫線のある表を再現"
    ko "선이 있는 표 복원";

Tables
    zh "还原无框线多列版面"
    ht "還原無框線多欄版面"
    en "Rebuild borderless multi-column layout"
    es "Reconstruir diseño multicolumna sin bordes"
    de "Randloses Mehrspalten-Layout rekonstruieren"
    ja "罫線のない多段組みを再現"
    ko "선 없는 다단 레이아웃 복원";

PageMarker
    zh "插「原第 N 页」标记"
    ht "插入「原第 N 頁」標記"
    en "Insert “page N of original” markers"
    es "Insertar marcas «página N del original»"
    de "Marken „Seite N des Originals“ einfügen"
    ja "「原文 N ページ」の目印を入れる"
    ko "「원본 N쪽」 표시 넣기";

DropHeader
    zh "去页眉"
    ht "去頁首"
    en "Drop headers"
    es "Quitar encabezados"
    de "Kopfzeilen entfernen"
    ja "ヘッダーを除く"
    ko "머리글 제거";

DropFooter
    zh "去页脚"
    ht "去頁尾"
    en "Drop footers"
    es "Quitar pies de página"
    de "Fußzeilen entfernen"
    ja "フッターを除く"
    ko "바닥글 제거";

DropStamp
    zh "去印章噪声"
    ht "去印章雜訊"
    en "Drop stamp noise"
    es "Quitar ruido de sellos"
    de "Stempelrauschen entfernen"
    ja "印影ノイズを除く"
    ko "도장 잡음 제거";

// ── 队列 ──

Queue
    zh "队列"
    ht "佇列"
    en "Queue"
    es "Cola"
    de "Warteschlange"
    ja "キュー"
    ko "대기열";

QueueCount
    zh "{0} 个"
    ht "{0} 個"
    en "{0}"
    es "{0}"
    de "{0}"
    ja "{0} 件"
    ko "{0}개";

QueueHint
    zh "PDF 或整个文件夹, 拖进窗口就行"
    ht "PDF 或整個資料夾，拖進視窗就行"
    en "Drop PDFs — or whole folders — anywhere in this window"
    es "Arrastra PDF, o carpetas enteras, a esta ventana"
    de "PDFs – oder ganze Ordner – einfach in dieses Fenster ziehen"
    ja "PDF でもフォルダごとでも、この窓にドロップするだけ"
    ko "PDF든 폴더째든 이 창에 끌어다 놓으면 됩니다";

QueueEmpty
    zh "队列是空的"
    ht "佇列是空的"
    en "Queue is empty"
    es "La cola está vacía"
    de "Warteschlange ist leer"
    ja "キューは空です"
    ko "대기열이 비어 있습니다";

OpenOutTip
    zh "点开转出来的文件"
    ht "點開轉出來的檔案"
    en "Open the converted file"
    es "Abrir el archivo convertido"
    de "Umgewandelte Datei öffnen"
    ja "変換後のファイルを開く"
    ko "변환된 파일 열기";

// ── 日志区 ──

LogTitle
    zh "日志"
    ht "紀錄"
    en "Log"
    es "Registro"
    de "Protokoll"
    ja "ログ"
    ko "로그";

Tip1
    zh "① 把扫描版 PDF 拖进左边, 或点「添加 PDF…」——整个文件夹也行"
    ht "① 把掃描版 PDF 拖進左邊，或點「新增 PDF…」——整個資料夾也行"
    en "① Drag scanned PDFs into the list on the left, or click “Add PDF…” — whole folders work too"
    es "① Arrastra los PDF escaneados a la lista de la izquierda, o pulsa «Añadir PDF…» — también valen carpetas enteras"
    de "① Gescannte PDFs in die Liste links ziehen oder auf „PDF hinzufügen…“ klicken – ganze Ordner gehen auch"
    ja "① スキャンした PDF を左の一覧にドラッグ、または「PDF を追加…」をクリック。フォルダごとでも構いません"
    ko "① 스캔한 PDF를 왼쪽 목록에 끌어다 놓거나 「PDF 추가…」를 누르세요. 폴더째도 됩니다";

Tip2
    zh "② 选输出格式: Word 保版式, Excel 只导表格; 队列里可以单独给某份另设"
    ht "② 選輸出格式：Word 保版面，Excel 只匯出表格；佇列裡可以單獨給某份另設"
    en "② Pick the output: Word keeps the layout, Excel exports tables only. Any single file can differ"
    es "② Elige la salida: Word conserva el diseño, Excel solo exporta tablas. Cada archivo puede tener la suya"
    de "② Ausgabe wählen: Word behält das Layout, Excel exportiert nur Tabellen. Einzelne Dateien dürfen abweichen"
    ja "② 出力形式を選びます。Word は版面を保ち、Excel は表だけ書き出します。ファイルごとに変えられます"
    ko "② 출력 형식을 고르세요. Word는 레이아웃을 살리고 Excel은 표만 내보냅니다. 파일마다 다르게 정해도 됩니다";

Tip3
    zh "③ 点「开始转换」(或直接敲回车)"
    ht "③ 點「開始轉換」(或直接按 Enter)"
    en "③ Click “Convert” (or just press Enter)"
    es "③ Pulsa «Convertir» (o simplemente Intro)"
    de "③ Auf „Umwandeln“ klicken (oder einfach Eingabetaste)"
    ja "③「変換開始」をクリック(Enter でも可)"
    ko "③「변환 시작」을 누르세요(Enter도 됩니다)";

TipLocal
    zh "全程在本机跑, 不联网, 文件不出这台电脑; 首次转换要加载模型, 慢一两秒"
    ht "全程在本機跑，不連網，檔案不出這台電腦；首次轉換要載入模型，慢一兩秒"
    en "Everything runs on this machine. Your files never leave it. The first run spends a second or two loading the models"
    es "Todo se ejecuta en este equipo; tus archivos no salen de él. La primera vez tarda uno o dos segundos en cargar los modelos"
    de "Alles läuft auf diesem Rechner, Ihre Dateien verlassen ihn nie. Der erste Lauf braucht ein bis zwei Sekunden zum Laden der Modelle"
    ja "すべてこのパソコンの中で処理します。ファイルは外に出ません。初回だけモデルの読み込みに 1〜2 秒かかります"
    ko "모두 이 컴퓨터 안에서 처리합니다. 파일은 밖으로 나가지 않습니다. 처음 한 번은 모델을 읽느라 1~2초 걸립니다";

DropOverlay
    zh "松手加入队列 ({0} 项)"
    ht "放開即加入佇列 ({0} 項)"
    en "Release to add ({0})"
    es "Suelta para añadir ({0})"
    de "Loslassen zum Hinzufügen ({0})"
    ja "離すとキューに追加 ({0} 件)"
    ko "놓으면 대기열에 추가 ({0}개)";

// ── 加文件时的回话 ──

Added
    zh "加入 {0} 个文件"
    ht "加入 {0} 個檔案"
    en "Added {0}"
    es "Añadidos: {0}"
    de "Hinzugefügt: {0}"
    ja "{0} 件を追加"
    ko "{0}개 추가";

AddedDup
    zh ", 跳过 {0} 个已在队列里的"
    ht "，略過 {0} 個已在佇列裡的"
    en ", skipped {0} already queued"
    es ", omitidos {0} ya en la cola"
    de ", {0} bereits in der Warteschlange übersprungen"
    ja "、既にキューにある {0} 件は省略"
    ko ", 이미 대기열에 있는 {0}개는 건너뜀";

AddedSkip
    zh ", 跳过 {0} 个不是 PDF 的"
    ht "，略過 {0} 個不是 PDF 的"
    en ", skipped {0} that are not PDFs"
    es ", omitidos {0} que no son PDF"
    de ", {0} Nicht-PDFs übersprungen"
    ja "、PDF でない {0} 件は省略"
    ko ", PDF가 아닌 {0}개는 건너뜀";

AddedRunning
    zh " —— 正在转换中, 这些要等下一轮"
    ht " —— 正在轉換中，這些要等下一輪"
    en " — a conversion is running, these wait for the next round"
    es " — hay una conversión en curso; estos esperan a la siguiente ronda"
    de " – es läuft gerade eine Umwandlung, diese warten auf die nächste Runde"
    ja " —— 変換中のため、これらは次回に回ります"
    ko " —— 변환 중이라 이번엔 빠지고 다음 차례로 넘어갑니다";

NoPdfIn
    zh "{0} 里没有 PDF"
    ht "{0} 裡沒有 PDF"
    en "No PDFs in {0}"
    es "No hay PDF en {0}"
    de "Keine PDFs in {0}"
    ja "{0} に PDF がありません"
    ko "{0} 안에 PDF가 없습니다";

WorkerDead
    zh "工作线程已退出, 请重启程序"
    ht "工作執行緒已結束，請重新啟動程式"
    en "The worker thread has exited — please restart the program"
    es "El hilo de trabajo terminó; reinicia el programa"
    de "Der Arbeits-Thread ist beendet – bitte das Programm neu starten"
    ja "ワーカースレッドが終了しました。プログラムを再起動してください"
    ko "작업 스레드가 종료됐습니다. 프로그램을 다시 시작하세요";

// ── 进度与汇总 ──

Elapsed
    zh " · 已用 {0}"
    ht " · 已用 {0}"
    en " · {0} elapsed"
    es " · {0} transcurridos"
    de " · {0} vergangen"
    ja " · 経過 {0}"
    ko " · {0} 경과";

Eta
    zh ", 约剩 {0}"
    ht "，約剩 {0}"
    en ", about {0} left"
    es ", quedan unos {0}"
    de ", noch etwa {0}"
    ja "、残り約 {0}"
    ko ", 약 {0} 남음";

SumDone
    zh "完成 {0} 个"
    ht "完成 {0} 個"
    en "Done: {0}"
    es "Terminados: {0}"
    de "Fertig: {0}"
    ja "完了 {0} 件"
    ko "완료 {0}개";

SumFailed
    zh ", 失败 {0} 个"
    ht "，失敗 {0} 個"
    en ", failed: {0}"
    es ", con errores: {0}"
    de ", fehlgeschlagen: {0}"
    ja "、失敗 {0} 件"
    ko ", 실패 {0}개";

SumLeft
    zh ", 未转 {0} 个"
    ht "，未轉 {0} 個"
    en ", not converted: {0}"
    es ", sin convertir: {0}"
    de ", nicht umgewandelt: {0}"
    ja "、未変換 {0} 件"
    ko ", 변환 안 함 {0}개";

SumTime
    zh " · 用时 {0}"
    ht " · 費時 {0}"
    en " · took {0}"
    es " · tardó {0}"
    de " · Dauer {0}"
    ja " · 所要 {0}"
    ko " · {0} 걸림";

Stopped
    zh "已停止"
    ht "已停止"
    en "Stopped"
    es "Detenido"
    de "Angehalten"
    ja "停止しました"
    ko "중지했습니다";

InitFailed
    zh "初始化失败: {0}"
    ht "初始化失敗：{0}"
    en "Startup failed: {0}"
    es "Fallo al iniciar: {0}"
    de "Start fehlgeschlagen: {0}"
    ja "初期化に失敗しました: {0}"
    ko "초기화 실패: {0}";

ConvFailed
    zh "  ✗ 转换失败: {0}"
    ht "  ✗ 轉換失敗：{0}"
    en "  ✗ Conversion failed: {0}"
    es "  ✗ Conversión fallida: {0}"
    de "  ✗ Umwandlung fehlgeschlagen: {0}"
    ja "  ✗ 変換に失敗しました: {0}"
    ko "  ✗ 변환 실패: {0}";

// 时长一律写成缩写, 躲开各语言的单复数规则 —— 计时器上没人在意
// "1 second" 还是 "1 seconds"
DurS
    zh "{0} 秒"
    ht "{0} 秒"
    en "{0}s"
    es "{0} s"
    de "{0} s"
    ja "{0} 秒"
    ko "{0}초";

DurM
    zh "{0} 分 {1} 秒"
    ht "{0} 分 {1} 秒"
    en "{0}m {1}s"
    es "{0} min {1} s"
    de "{0} min {1} s"
    ja "{0} 分 {1} 秒"
    ko "{0}분 {1}초";

DurH
    zh "{0} 小时 {1} 分"
    ht "{0} 小時 {1} 分"
    en "{0}h {1}m"
    es "{0} h {1} min"
    de "{0} Std. {1} min"
    ja "{0} 時間 {1} 分"
    ko "{0}시간 {1}분";

// ── 语言 ──

UiLang
    zh "界面语言"
    ht "介面語言"
    en "Interface language"
    es "Idioma de la interfaz"
    de "Sprache der Oberfläche"
    ja "表示言語"
    ko "화면 언어";

OcrLang
    zh "识别语言"
    ht "辨識語言"
    en "Recognition"
    es "Reconocimiento"
    de "Erkennung"
    ja "認識する言語"
    ko "인식 언어";

OcrLangTip
    zh "识别语言决定认得出哪些字, 跟界面语言是两回事。一次只认一种 —— 选了韩语, 同一份文件里的汉字就认不出来了。"
    ht "辨識語言決定認得出哪些字，跟介面語言是兩回事。一次只認一種 —— 選了韓語，同一份檔案裡的漢字就認不出來了。"
    en "The recognition language decides which characters can be read. It has nothing to do with the interface language. Only one at a time — pick Korean and Chinese characters in the same file will not be recognised."
    es "El idioma de reconocimiento decide qué caracteres se pueden leer; no tiene nada que ver con el idioma de la interfaz. Solo uno a la vez: si eliges coreano, los caracteres chinos del mismo archivo no se reconocerán."
    de "Die Erkennungssprache bestimmt, welche Zeichen gelesen werden können – mit der Sprache der Oberfläche hat das nichts zu tun. Immer nur eine: Wer Koreanisch wählt, bei dem werden chinesische Zeichen in derselben Datei nicht erkannt."
    ja "認識する言語によって、読み取れる文字が決まります。表示言語とは別のものです。一度に一種類だけ —— 韓国語を選ぶと、同じファイル内の漢字は読み取れません。"
    ko "인식 언어는 어떤 글자를 읽을 수 있는지를 정합니다. 화면 언어와는 별개입니다. 한 번에 한 가지만 — 한국어를 고르면 같은 파일 안의 한자는 인식하지 못합니다.";

OcrBuiltin
    zh "中文 · 英文"
    ht "中文 · 英文"
    en "Chinese · English"
    es "Chino · Inglés"
    de "Chinesisch · Englisch"
    ja "中国語 · 英語"
    ko "중국어 · 영어";

OcrBuiltinNote
    zh "简繁中文、英文、日文假名、拉丁字母和希腊字母"
    ht "繁簡中文、英文、日文假名、拉丁字母和希臘字母"
    en "Chinese, English, Japanese kana, Latin and Greek letters"
    es "Chino, inglés, kana japonés, alfabeto latino y griego"
    de "Chinesisch, Englisch, japanische Kana, lateinische und griechische Buchstaben"
    ja "中国語、英語、日本語のかな、ラテン文字、ギリシャ文字"
    ko "중국어, 영어, 일본어 가나, 라틴 문자, 그리스 문자";

OcrKo
    zh "韩语"
    ht "韓語"
    en "Korean"
    es "Coreano"
    de "Koreanisch"
    ja "韓国語"
    ko "한국어";

OcrKoNote
    zh "谚文。内置字库一个韩文字都不认"
    ht "諺文。內建字庫一個韓文字都不認"
    en "Hangul. The built-in set holds no Korean at all"
    es "Hangul. El juego incluido no reconoce ningún carácter coreano"
    de "Hangul. Der eingebaute Zeichensatz kennt kein einziges koreanisches Zeichen"
    ja "ハングル。内蔵の文字セットには韓国語が一文字も入っていません"
    ko "한글. 기본 글자표에는 한글이 하나도 없습니다";

OcrJa
    zh "日语"
    ht "日語"
    en "Japanese"
    es "Japonés"
    de "Japanisch"
    ja "日本語"
    ko "일본어";

OcrJaNote
    zh "内置就认假名和汉字。只有内置认不好时才需要它"
    ht "內建就認假名和漢字。只有內建認不好時才需要它"
    en "Kana and kanji already work built in. Only worth it if they come out wrong"
    es "El kana y los kanji ya funcionan con el juego incluido. Solo vale la pena si salen mal"
    de "Kana und Kanji gehen schon eingebaut. Nur nötig, wenn sie falsch herauskommen"
    ja "かなと漢字は内蔵のままで読めます。うまく読めないときだけ"
    ko "가나와 한자는 기본으로도 읽힙니다. 잘 안 읽힐 때만 받으세요";

OcrRu
    zh "俄语等西里尔文"
    ht "俄語等西里爾文"
    en "Russian and Cyrillic"
    es "Ruso y cirílico"
    de "Russisch und Kyrillisch"
    ja "ロシア語・キリル文字"
    ko "러시아어·키릴 문자";

OcrRuNote
    zh "俄语、乌克兰语、塞尔维亚语等"
    ht "俄語、烏克蘭語、塞爾維亞語等"
    en "Russian, Ukrainian, Serbian and more"
    es "Ruso, ucraniano, serbio y más"
    de "Russisch, Ukrainisch, Serbisch und mehr"
    ja "ロシア語、ウクライナ語、セルビア語など"
    ko "러시아어, 우크라이나어, 세르비아어 등";

OcrAr
    zh "阿拉伯语"
    ht "阿拉伯語"
    en "Arabic"
    es "Árabe"
    de "Arabisch"
    ja "アラビア語"
    ko "아랍어";

OcrArNote
    zh "阿拉伯语、波斯语、乌尔都语等"
    ht "阿拉伯語、波斯語、烏爾都語等"
    en "Arabic, Persian, Urdu and more"
    es "Árabe, persa, urdu y más"
    de "Arabisch, Persisch, Urdu und mehr"
    ja "アラビア語、ペルシャ語、ウルドゥー語など"
    ko "아랍어, 페르시아어, 우르두어 등";

OcrTh
    zh "泰语"
    ht "泰語"
    en "Thai"
    es "Tailandés"
    de "Thailändisch"
    ja "タイ語"
    ko "태국어";

OcrThNote
    zh "泰文"
    ht "泰文"
    en "Thai script"
    es "Escritura tailandesa"
    de "Thailändische Schrift"
    ja "タイ文字"
    ko "태국 문자";

OcrHi
    zh "印地语等天城文"
    ht "印地語等天城文"
    en "Hindi and Devanagari"
    es "Hindi y devanagari"
    de "Hindi und Devanagari"
    ja "ヒンディー語・デーヴァナーガリー"
    ko "힌디어·데바나가리";

OcrHiNote
    zh "印地语、马拉地语、尼泊尔语等"
    ht "印地語、馬拉地語、尼泊爾語等"
    en "Hindi, Marathi, Nepali and more"
    es "Hindi, maratí, nepalí y más"
    de "Hindi, Marathi, Nepali und mehr"
    ja "ヒンディー語、マラーティー語、ネパール語など"
    ko "힌디어, 마라티어, 네팔어 등";

// ── 语言包 ──

PackNeedsDownload
    zh "要先下载 {0}"
    ht "要先下載 {0}"
    en "needs a {0} download"
    es "requiere descargar {0}"
    de "muss noch geladen werden ({0})"
    ja "ダウンロードが必要 ({0})"
    ko "내려받아야 함 ({0})";

PackDownloading
    zh "正在下载{0}语言包 {1}%"
    ht "正在下載{0}語言包 {1}%"
    en "Downloading {0} pack… {1}%"
    es "Descargando el paquete de {0}… {1} %"
    de "{0}-Paket wird geladen … {1} %"
    ja "{0}の言語パックをダウンロード中 {1}%"
    ko "{0} 언어 팩 다운로드 중 {1}%";

PackFailed
    zh "{0}语言包下载不了：{1}"
    ht "{0}語言包下載不了：{1}"
    en "Couldn’t download the {0} pack: {1}"
    es "No se pudo descargar el paquete de {0}: {1}"
    de "Das {0}-Paket konnte nicht geladen werden: {1}"
    ja "{0}の言語パックをダウンロードできませんでした：{1}"
    ko "{0} 언어 팩을 내려받지 못했습니다: {1}";

PackReady
    zh "识别语言已改成{0}"
    ht "辨識語言已改成{0}"
    en "Now reading {0}"
    es "Ahora se reconoce {0}"
    de "Erkennt jetzt {0}"
    ja "{0}で認識します"
    ko "이제 {0}(으)로 인식합니다";

PackNoCurl
    zh "这台机器上没有 curl, 没法下载语言包"
    ht "這台機器上沒有 curl，沒法下載語言包"
    en "curl is not available on this machine, so language packs cannot be downloaded"
    es "curl no está disponible en este equipo, así que no se pueden descargar paquetes de idioma"
    de "curl ist auf diesem Rechner nicht vorhanden, daher lassen sich keine Sprachpakete laden"
    ja "このパソコンに curl がないため、言語パックをダウンロードできません"
    ko "이 컴퓨터에 curl이 없어 언어 팩을 내려받을 수 없습니다";

PackHttpFailed
    zh "下载失败(curl 退出码 {0})"
    ht "下載失敗(curl 結束碼 {0})"
    en "Download failed (curl exit code {0})"
    es "Descarga fallida (código de salida de curl: {0})"
    de "Download fehlgeschlagen (curl-Exitcode {0})"
    ja "ダウンロードに失敗しました(curl 終了コード {0})"
    ko "다운로드 실패(curl 종료 코드 {0})";

PackShort
    zh "下载不完整: {0} / {1} 字节"
    ht "下載不完整：{0} / {1} 位元組"
    en "Incomplete download: {0} of {1} bytes"
    es "Descarga incompleta: {0} de {1} bytes"
    de "Unvollständiger Download: {0} von {1} Bytes"
    ja "ダウンロードが不完全です: {0} / {1} バイト"
    ko "다운로드가 불완전합니다: {0} / {1} 바이트";

PackBadHash
    zh "文件校验不过: {0}"
    ht "檔案校驗不過：{0}"
    en "Checksum mismatch: {0}"
    es "La suma de comprobación no coincide: {0}"
    de "Prüfsumme stimmt nicht: {0}"
    ja "チェックサムが一致しません: {0}"
    ko "체크섬이 맞지 않습니다: {0}";

PackNoDir
    zh "找不到能写的目录来存语言包"
    ht "找不到能寫的目錄來存語言包"
    en "No writable folder to keep language packs in"
    es "No hay una carpeta con permiso de escritura para los paquetes de idioma"
    de "Kein beschreibbarer Ordner für Sprachpakete gefunden"
    ja "言語パックを置ける書き込み可能なフォルダが見つかりません"
    ko "언어 팩을 저장할 쓰기 가능한 폴더가 없습니다";

// ── 转换过程(库) ──

LibStart
    zh "[{0}] {1} 页, 识别中(首次启动稍慢)..."
    ht "[{0}] {1} 頁，辨識中(首次啟動稍慢)..."
    en "[{0}] {1} pages, recognising (the first run is slower)…"
    es "[{0}] {1} páginas, reconociendo (la primera vez es más lenta)…"
    de "[{0}] {1} Seiten, Erkennung läuft (der erste Lauf dauert länger) …"
    ja "[{0}] {1} ページ、認識中(初回はやや遅くなります)…"
    ko "[{0}] {1}쪽, 인식 중(처음 한 번은 조금 느립니다)…";

LibPage
    zh "  第 {0} 页: {1} 行文字, {2} 个块 (无框线表 {3} 张, 框线表 {4} 张), 页眉页脚 {5} 行"
    ht "  第 {0} 頁：{1} 行文字，{2} 個區塊 (無框線表 {3} 張, 框線表 {4} 張)，頁首頁尾 {5} 行"
    en "  page {0}: {1} text lines, {2} blocks ({3} borderless tables, {4} ruled tables), {5} header/footer lines"
    es "  página {0}: {1} líneas de texto, {2} bloques ({3} tablas sin bordes, {4} tablas con líneas), {5} líneas de encabezado/pie"
    de "  Seite {0}: {1} Textzeilen, {2} Blöcke ({3} randlose Tabellen, {4} Tabellen mit Linien), {5} Kopf-/Fußzeilen"
    ja "  {0} ページ: 文字 {1} 行、ブロック {2} 個(罫線なしの表 {3}、罫線ありの表 {4})、ヘッダー・フッター {5} 行"
    ko "  {0}쪽: 글자 {1}줄, 블록 {2}개(선 없는 표 {3}, 선 있는 표 {4}), 머리글·바닥글 {5}줄";

LibFigures
    zh "  第 {0} 页: 保留了 {1} 处印章/签名/插图"
    ht "  第 {0} 頁：保留了 {1} 處印章/簽名/插圖"
    en "  page {0}: kept {1} stamps/signatures/figures"
    es "  página {0}: se han conservado {1} sellos/firmas/figuras"
    de "  Seite {0}: {1} Stempel/Unterschriften/Abbildungen übernommen"
    ja "  {0} ページ: 印章・署名・図を {1} 箇所そのまま残しました"
    ko "  {0}쪽: 도장·서명·그림 {1}곳을 그대로 남겼습니다";

LibFlatten
    zh "  第 {0} 页: 光照差 {1} 级, 已摊平"
    ht "  第 {0} 頁：光照差 {1} 級，已攤平"
    en "  page {0}: lighting varied by {1} levels, evened out"
    es "  página {0}: la luz variaba {1} niveles, se ha igualado"
    de "  Seite {0}: Beleuchtung schwankte um {1} Stufen, ausgeglichen"
    ja "  {0} ページ: 明るさが {1} 段ばらついていたので、平らにしました"
    ko "  {0}쪽: 밝기가 {1}단계 고르지 않아 평탄화했습니다";

LibDeskew
    zh "  第 {0} 页: 扫歪了 {1}°, 已转正"
    ht "  第 {0} 頁：掃歪了 {1}°，已轉正"
    en "  page {0}: scanned {1}° off, straightened"
    es "  página {0}: escaneada {1}° torcida, enderezada"
    de "  Seite {0}: {1}° schief eingescannt, gerade gerückt"
    ja "  {0} ページ: {1}° 傾いて取り込まれていたので、まっすぐにしました"
    ko "  {0}쪽: {1}° 기울어져 스캔되어 바로잡았습니다";

LibTextLayer
    zh "  第 {0} 页: PDF 自带文字层, 直接取字, 不识别"
    ht "  第 {0} 頁：PDF 自帶文字層，直接取字，不辨識"
    en "  page {0}: has its own text layer, taking the text as-is instead of recognising it"
    es "  página {0}: tiene capa de texto propia; se toma el texto tal cual en vez de reconocerlo"
    de "  Seite {0}: hat eine eigene Textebene; der Text wird direkt übernommen statt erkannt"
    ja "  {0} ページ: PDF 自体の文字レイヤーがあるので、認識せずそのまま取り出します"
    ko "  {0}쪽: PDF 자체 텍스트 레이어가 있어 인식하지 않고 그대로 가져옵니다";

LibPageFailed
    zh "  ! 第 {0} 页失败, 已跳过: {1}"
    ht "  ! 第 {0} 頁失敗，已略過：{1}"
    en "  ! page {0} failed and was skipped: {1}"
    es "  ! la página {0} falló y se omitió: {1}"
    de "  ! Seite {0} fehlgeschlagen und übersprungen: {1}"
    ja "  ! {0} ページは失敗したため飛ばしました: {1}"
    ko "  ! {0}쪽은 실패해서 건너뛰었습니다: {1}";

LibOutDocx
    zh "  ✓ {0}  ({1} 页, {2} KB)"
    ht "  ✓ {0}  ({1} 頁, {2} KB)"
    en "  ✓ {0}  ({1} pages, {2} KB)"
    es "  ✓ {0}  ({1} páginas, {2} KB)"
    de "  ✓ {0}  ({1} Seiten, {2} KB)"
    ja "  ✓ {0}  ({1} ページ, {2} KB)"
    ko "  ✓ {0}  ({1}쪽, {2} KB)";

LibOutXlsx
    zh "  ✓ {0}  (共导出 {1} 张表格, {2} KB)"
    ht "  ✓ {0}  (共匯出 {1} 張表格, {2} KB)"
    en "  ✓ {0}  ({1} tables exported, {2} KB)"
    es "  ✓ {0}  ({1} tablas exportadas, {2} KB)"
    de "  ✓ {0}  ({1} Tabellen exportiert, {2} KB)"
    ja "  ✓ {0}  (表 {1} 個を書き出し, {2} KB)"
    ko "  ✓ {0}  (표 {1}개 내보냄, {2} KB)";

LibFileErrors
    zh "  ! 本文件有 {0} 处失败(已跳过)"
    ht "  ! 本檔案有 {0} 處失敗(已略過)"
    en "  ! {0} failures in this file (skipped)"
    es "  ! {0} fallos en este archivo (omitidos)"
    de "  ! {0} Fehler in dieser Datei (übersprungen)"
    ja "  ! このファイルで {0} 件失敗しました(飛ばしました)"
    ko "  ! 이 파일에서 {0}건 실패(건너뜀)";

LibExists
    zh "  ↷ {0} 已存在, 不覆盖, 另存为 {1}"
    ht "  ↷ {0} 已存在，不覆蓋，另存為 {1}"
    en "  ↷ {0} already exists — not overwriting, saved as {1} instead"
    es "  ↷ {0} ya existe; no se sobrescribe, se guardó como {1}"
    de "  ↷ {0} existiert bereits – wird nicht überschrieben, stattdessen als {1} gespeichert"
    ja "  ↷ {0} は既にあります。上書きせず {1} として保存しました"
    ko "  ↷ {0} 이(가) 이미 있어 덮어쓰지 않고 {1} 로 저장했습니다";

LibCancelled
    zh "已取消"
    ht "已取消"
    en "Cancelled"
    es "Cancelado"
    de "Abgebrochen"
    ja "中止しました"
    ko "취소했습니다";

LibNoFile
    zh "文件不存在: {0}"
    ht "檔案不存在：{0}"
    en "No such file: {0}"
    es "No existe el archivo: {0}"
    de "Datei nicht vorhanden: {0}"
    ja "ファイルがありません: {0}"
    ko "파일이 없습니다: {0}";

LibNoPages
    zh "没有任何页面, 文件可能已损坏或被加密"
    ht "沒有任何頁面，檔案可能已損毀或被加密"
    en "No pages at all — the file may be damaged or encrypted"
    es "No hay ninguna página: el archivo puede estar dañado o cifrado"
    de "Überhaupt keine Seiten – die Datei ist womöglich beschädigt oder verschlüsselt"
    ja "ページが一つもありません。ファイルが壊れているか暗号化されている可能性があります"
    ko "페이지가 하나도 없습니다. 파일이 손상됐거나 암호화됐을 수 있습니다";

LibRenderPage
    zh "渲染第 {0} 页"
    ht "算圖第 {0} 頁"
    en "rendering page {0}"
    es "renderizando la página {0}"
    de "Seite {0} wird gerendert"
    ja "{0} ページを描画"
    ko "{0}쪽 렌더링";

LibOcrPageCtx
    zh "识别第 {0} 页"
    ht "辨識第 {0} 頁"
    en "recognising page {0}"
    es "reconociendo la página {0}"
    de "Seite {0} wird erkannt"
    ja "{0} ページを認識"
    ko "{0}쪽 인식";

LibStageOcr
    zh "识别 {0}/{1}"
    ht "辨識 {0}/{1}"
    en "recognising {0}/{1}"
    es "reconociendo {0}/{1}"
    de "Erkennung {0}/{1}"
    ja "認識 {0}/{1}"
    ko "인식 {0}/{1}";

LibStageWrite
    zh "写出文件"
    ht "寫出檔案"
    en "writing files"
    es "escribiendo archivos"
    de "Dateien werden geschrieben"
    ja "ファイルを書き出し中"
    ko "파일 쓰는 중";

LibStageLayout
    zh "版面重建"
    ht "版面重建"
    en "layout"
    es "diseño"
    de "Layout"
    ja "レイアウト再構成"
    ko "레이아웃 재구성";

LibNoModels
    zh "找不到 OCR 模型。把三个 .onnx 放进程序同目录的 models/, 或用环境变量 PDF2DOC_MODELS 指定。"
    ht "找不到 OCR 模型。把三個 .onnx 放進程式同目錄的 models/，或用環境變數 PDF2DOC_MODELS 指定。"
    en "OCR models not found. Put the three .onnx files in a models/ folder next to the program, or point PDF2DOC_MODELS at them."
    es "No se encontraron los modelos de OCR. Coloca los tres archivos .onnx en una carpeta models/ junto al programa, o indica su ruta con PDF2DOC_MODELS."
    de "OCR-Modelle nicht gefunden. Legen Sie die drei .onnx-Dateien in einen Ordner models/ neben das Programm oder verweisen Sie mit PDF2DOC_MODELS darauf."
    ja "OCR モデルが見つかりません。3 つの .onnx をプログラムと同じ場所の models/ に置くか、環境変数 PDF2DOC_MODELS で指定してください。"
    ko "OCR 모델을 찾을 수 없습니다. .onnx 파일 3개를 프로그램과 같은 위치의 models/ 에 두거나 환경 변수 PDF2DOC_MODELS 로 알려 주세요.";

// ── 命令行 ──

CliAbout
    zh "扫描版 PDF -> 保版式可编辑 Word / Excel 表格 (全本地, 无需联网)"
    ht "掃描版 PDF -> 保版面可編輯 Word / Excel 表格 (全本機，無需連網)"
    en "Scanned PDF -> editable Word with the layout kept, or Excel tables (fully local, no network)"
    es "PDF escaneado -> Word editable conservando el diseño, o tablas de Excel (todo local, sin red)"
    de "Gescanntes PDF -> bearbeitbares Word mit erhaltenem Layout oder Excel-Tabellen (komplett lokal, ohne Netz)"
    ja "スキャンした PDF -> 版面を保った編集可能な Word / Excel の表(すべてローカル、ネット不要)"
    ko "스캔한 PDF -> 레이아웃을 살린 편집 가능한 Word 또는 Excel 표(전부 로컬, 인터넷 불필요)";

CliUsage
    zh "用法: pdf2doc <文件.pdf> [...]  (更多选项见 --help)"
    ht "用法：pdf2doc <檔案.pdf> [...]  (更多選項見 --help)"
    en "Usage: pdf2doc <file.pdf> [...]   (see --help for more)"
    es "Uso: pdf2doc <archivo.pdf> [...]   (más opciones en --help)"
    de "Aufruf: pdf2doc <Datei.pdf> [...]   (weitere Optionen unter --help)"
    ja "使い方: pdf2doc <ファイル.pdf> [...]  (詳しくは --help)"
    ko "사용법: pdf2doc <파일.pdf> [...]  (자세한 건 --help)";

CliBadFormat
    zh "--to 只能是 docx / xlsx / both, 收到 {0}"
    ht "--to 只能是 docx / xlsx / both，收到 {0}"
    en "--to must be docx / xlsx / both, got {0}"
    es "--to debe ser docx / xlsx / both; se recibió {0}"
    de "--to muss docx / xlsx / both sein, erhalten: {0}"
    ja "--to は docx / xlsx / both のいずれかです。受け取ったのは {0}"
    ko "--to 는 docx / xlsx / both 중 하나여야 합니다. 받은 값: {0}";

CliBadLang
    zh "--lang 只能是 {0}, 收到 {1}"
    ht "--lang 只能是 {0}，收到 {1}"
    en "--lang must be one of {0}, got {1}"
    es "--lang debe ser uno de {0}; se recibió {1}"
    de "--lang muss eines von {0} sein, erhalten: {1}"
    ja "--lang は {0} のいずれかです。受け取ったのは {1}"
    ko "--lang 은 {0} 중 하나여야 합니다. 받은 값: {1}";

CliBadOcrLang
    zh "--ocr-lang 只能是 {0}, 收到 {1}"
    ht "--ocr-lang 只能是 {0}，收到 {1}"
    en "--ocr-lang must be one of {0}, got {1}"
    es "--ocr-lang debe ser uno de {0}; se recibió {1}"
    de "--ocr-lang muss eines von {0} sein, erhalten: {1}"
    ja "--ocr-lang は {0} のいずれかです。受け取ったのは {1}"
    ko "--ocr-lang 은 {0} 중 하나여야 합니다. 받은 값: {1}";

CliConvFailed
    zh "  ✗ 转换失败 {0}: {1}"
    ht "  ✗ 轉換失敗 {0}：{1}"
    en "  ✗ Conversion failed for {0}: {1}"
    es "  ✗ Falló la conversión de {0}: {1}"
    de "  ✗ Umwandlung von {0} fehlgeschlagen: {1}"
    ja "  ✗ {0} の変換に失敗しました: {1}"
    ko "  ✗ {0} 변환 실패: {1}";

CliErrSummary
    zh "--- 错误汇总 ---"
    ht "--- 錯誤彙總 ---"
    en "--- errors ---"
    es "--- errores ---"
    de "--- Fehler ---"
    ja "--- エラー一覧 ---"
    ko "--- 오류 모음 ---";

CliWholeFailed
    zh "  整篇失败: {0}"
    ht "  整份失敗：{0}"
    en "  whole file failed: {0}"
    es "  archivo completo fallido: {0}"
    de "  gesamte Datei fehlgeschlagen: {0}"
    ja "  ファイル全体が失敗: {0}"
    ko "  파일 전체 실패: {0}";

CliPageFailed
    zh "  {0} 第 {1} 页 {2}: {3}"
    ht "  {0} 第 {1} 頁 {2}：{3}"
    en "  {0} page {1} ({2}): {3}"
    es "  {0} página {1} ({2}): {3}"
    de "  {0} Seite {1} ({2}): {3}"
    ja "  {0} {1} ページ ({2}): {3}"
    ko "  {0} {1}쪽 ({2}): {3}";

HelpPdfs
    zh "待转换的扫描版 PDF"
    ht "待轉換的掃描版 PDF"
    en "the scanned PDFs to convert"
    es "los PDF escaneados que se van a convertir"
    de "die zu konvertierenden gescannten PDFs"
    ja "変換するスキャン PDF"
    ko "변환할 스캔 PDF";

HelpTo
    zh "输出格式: docx(默认, 保版式) / xlsx(只导表格) / both"
    ht "輸出格式：docx(預設，保版面) / xlsx(只匯出表格) / both"
    en "output: docx (default, keeps layout) / xlsx (tables only) / both"
    es "salida: docx (predet., conserva el diseño) / xlsx (solo tablas) / both"
    de "Ausgabe: docx (Standard, behält Layout) / xlsx (nur Tabellen) / both"
    ja "出力形式: docx(既定、版面を保つ) / xlsx(表のみ) / both"
    ko "출력 형식: docx(기본, 레이아웃 유지) / xlsx(표만) / both";

HelpOut
    zh "输出目录, 默认与源文件同目录"
    ht "輸出目錄，預設與原始檔案同目錄"
    en "output folder; defaults to the source file's folder"
    es "carpeta de salida; por omisión, la del archivo original"
    de "Zielordner; standardmäßig der Ordner der Quelldatei"
    ja "出力先フォルダ。既定は元のファイルと同じ場所"
    ko "출력 폴더. 기본값은 원본 파일과 같은 폴더";

HelpLongEdge
    zh "渲染长边像素, 越大越准也越慢(实际 dpi 由页面尺寸倒推, 夹在 150~300)"
    ht "算圖長邊像素，越大越準也越慢(實際 dpi 由頁面尺寸回推，夾在 150~300)"
    en "render long edge in pixels; larger is more accurate and slower (real dpi is derived from the page size, clamped to 150-300)"
    es "lado largo del render en píxeles; más grande es más preciso y más lento (el dpi real se deduce del tamaño de página, limitado a 150-300)"
    de "lange Kante beim Rendern in Pixeln; größer heißt genauer und langsamer (die tatsächliche dpi ergibt sich aus der Seitengröße, 150-300)"
    ja "レンダリング長辺のピクセル数。大きいほど正確だが遅い(実際の dpi はページ寸法から逆算し 150〜300)"
    ko "렌더링 긴 변의 픽셀 수. 클수록 정확하지만 느립니다(실제 dpi는 페이지 크기에서 역산해 150~300)";

HelpNoTables
    zh "不还原无框线的多列版面"
    ht "不還原無框線的多欄版面"
    en "do not rebuild borderless multi-column layout"
    es "no reconstruir el diseño multicolumna sin bordes"
    de "randloses Mehrspalten-Layout nicht rekonstruieren"
    ja "罫線のない多段組みを再現しない"
    ko "선 없는 다단 레이아웃을 복원하지 않음";

HelpNoGrid
    zh "不按框线还原表格"
    ht "不依框線還原表格"
    en "do not rebuild ruled tables"
    es "no reconstruir las tablas con líneas"
    de "Tabellen mit Linien nicht rekonstruieren"
    ja "罫線のある表を再現しない"
    ko "선이 있는 표를 복원하지 않음";

HelpNoMarker
    zh "不插「原第 N 页」标记"
    ht "不插入「原第 N 頁」標記"
    en "do not insert “page N of original” markers"
    es "no insertar marcas «página N del original»"
    de "keine Marken „Seite N des Originals“ einfügen"
    ja "「原文 N ページ」の目印を入れない"
    ko "「원본 N쪽」 표시를 넣지 않음";

HelpNoTextLayer
    zh "不用 PDF 自带的文字层, 一律识别(扫描件被塞过不准的文字层时用)"
    ht "不用 PDF 自帶的文字層，一律辨識(掃描件被塞過不準的文字層時用)"
    en "Ignore the PDF's own text layer and recognise everything (for scans carrying a bad text layer)"
    es "Ignorar la capa de texto del PDF y reconocerlo todo (para escaneos con una capa de texto defectuosa)"
    de "Die eigene Textebene des PDFs ignorieren und alles erkennen (für Scans mit fehlerhafter Textebene)"
    ja "PDF 自体の文字レイヤーを使わず、すべて認識する(不正確な文字レイヤーが入ったスキャン用)"
    ko "PDF 자체 텍스트 레이어를 쓰지 않고 모두 인식합니다(부정확한 텍스트 레이어가 든 스캔본용)";

HelpModels
    zh "OCR 模型目录, 默认自动查找"
    ht "OCR 模型目錄，預設自動尋找"
    en "OCR model folder; found automatically by default"
    es "carpeta de modelos de OCR; se busca automáticamente por omisión"
    de "Ordner mit den OCR-Modellen; wird standardmäßig automatisch gesucht"
    ja "OCR モデルのフォルダ。既定では自動で探します"
    ko "OCR 모델 폴더. 기본값은 자동 탐색";

HelpQuiet
    zh "只打结果, 不打逐页统计"
    ht "只印結果，不印逐頁統計"
    en "print results only, no per-page stats"
    es "imprimir solo los resultados, sin estadísticas por página"
    de "nur Ergebnisse ausgeben, keine Statistik je Seite"
    ja "結果だけ出力し、ページごとの統計は出さない"
    ko "결과만 출력하고 쪽별 통계는 생략";

HelpLang
    zh "界面语言, 默认跟系统走"
    ht "介面語言，預設跟系統走"
    en "interface language; follows the system by default"
    es "idioma de la interfaz; por omisión sigue al sistema"
    de "Sprache der Ausgabe; folgt standardmäßig dem System"
    ja "表示言語。既定ではシステムに従います"
    ko "화면 언어. 기본값은 시스템을 따름";

HelpOcrLang
    zh "识别语言, 缺语言包时自动下载; 默认内置的中英"
    ht "辨識語言，缺語言包時自動下載；預設內建的中英"
    en "recognition language; the pack is downloaded when missing. Defaults to the built-in Chinese + English"
    es "idioma de reconocimiento; el paquete se descarga si falta. Por omisión, el chino + inglés incluido"
    de "Erkennungssprache; das Paket wird bei Bedarf geladen. Standard ist das eingebaute Chinesisch + Englisch"
    ja "認識する言語。パックがなければ自動で取得します。既定は内蔵の中国語＋英語"
    ko "인식 언어. 팩이 없으면 자동으로 내려받습니다. 기본값은 내장된 중국어+영어";

// ── 写进产物里的字 ──
//
// 上面那些是界面上的, 关掉程序就没了; 这几条不一样 —— 它们印在转出来的
// Word / Excel 里, 会跟着文件一路传下去。所以宁可跟界面语言走: 谁转的文件
// 谁最可能第一个读它。

MarkPage
    zh "—— 原第 {0} 页 ——"
    ht "—— 原第 {0} 頁 ——"
    en "—— page {0} of the original ——"
    es "—— página {0} del original ——"
    de "—— Seite {0} des Originals ——"
    ja "—— 原文 {0} ページ ——"
    ko "—— 원본 {0}쪽 ——";

MarkPages
    zh "—— 原第 {0}–{1} 页 ——"
    ht "—— 原第 {0}–{1} 頁 ——"
    en "—— pages {0}–{1} of the original ——"
    es "—— páginas {0}–{1} del original ——"
    de "—— Seiten {0}–{1} des Originals ——"
    ja "—— 原文 {0}–{1} ページ ——"
    ko "—— 원본 {0}–{1}쪽 ——";

MarkFailed
    zh "[第 {0} 页解析失败, 已跳过: {1}]"
    ht "[第 {0} 頁解析失敗，已略過：{1}]"
    en "[page {0} failed and was skipped: {1}]"
    es "[la página {0} falló y se omitió: {1}]"
    de "[Seite {0} fehlgeschlagen und übersprungen: {1}]"
    ja "[{0} ページの解析に失敗したため飛ばしました: {1}]"
    ko "[{0}쪽 처리에 실패해 건너뛰었습니다: {1}]";

SheetName
    zh "表格"
    ht "表格"
    en "Tables"
    es "Tablas"
    de "Tabellen"
    ja "表"
    ko "표";

NoTableFound
    zh "（本文件没有识别到表格。整页版面请用 Word 输出。）"
    ht "（本檔案沒有辨識到表格。整頁版面請用 Word 輸出。）"
    en "(No tables were found in this file. Use the Word output for full-page layout.)"
    es "(No se encontraron tablas en este archivo. Usa la salida de Word para el diseño de página completa.)"
    de "(In dieser Datei wurden keine Tabellen gefunden. Für das ganzseitige Layout die Word-Ausgabe verwenden.)"
    ja "（このファイルからは表が見つかりませんでした。ページ全体の版面は Word 出力をお使いください。）"
    ko "(이 파일에서는 표를 찾지 못했습니다. 페이지 전체 레이아웃은 Word 출력을 쓰세요.)";

}

/// `LibPage` 那条参数最多, 单拎一个函数出来省得调用点排一长串 to_string
pub fn lib_page(
    no: usize,
    lines: usize,
    blocks: usize,
    tbl: usize,
    grid: usize,
    hf: usize,
) -> String {
    fill(
        t(K::LibPage),
        &[
            &no.to_string(),
            &lines.to_string(),
            &blocks.to_string(),
            &tbl.to_string(),
            &grid.to_string(),
            &hf.to_string(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_handles_reordering_and_multibyte() {
        assert_eq!(fill("{1} 在 {0} 后面", &["甲", "乙"]), "乙 在 甲 后面");
        // 缺的参数留空, 不 panic —— 少一个字总好过界面崩掉
        assert_eq!(fill("{0}{3}", &["x"]), "x");
        // 不是占位符的花括号原样留着
        assert_eq!(fill("{ } {a}", &[]), "{ } {a}");
    }

    #[test]
    fn locale_strings_map_to_the_right_language() {
        assert_eq!(Lang::parse("zh_CN.UTF-8"), Some(Lang::Zh));
        assert_eq!(Lang::parse("zh-Hant-TW"), Some(Lang::Hant));
        assert_eq!(Lang::parse("zh_TW"), Some(Lang::Hant));
        assert_eq!(Lang::parse("zh_HK"), Some(Lang::Hant));
        // 只看第一段, 后面的地区和修饰符怎么写都不影响
        assert_eq!(Lang::parse("de_DE@euro"), Some(Lang::De));
        assert_eq!(Lang::parse("de-DE"), Some(Lang::De));
        assert_eq!(Lang::parse("ko"), Some(Lang::Ko));
        assert_eq!(Lang::parse("fr_FR"), None);
    }

    /// 每种语言都得给全 —— 宏本身已经强制了, 这里再盯一眼"没有把中文原文
    /// 直接抄进其他语言"这类明显的漏译
    #[test]
    fn no_language_falls_back_to_chinese_by_accident() {
        for k in [K::Start, K::Queue, K::LogTitle, K::Ready, K::Advanced] {
            let zh = k.raw(Lang::Zh);
            for l in [Lang::En, Lang::Es, Lang::De, Lang::Ko] {
                assert_ne!(k.raw(l), zh, "{:?} 的 {:?} 还是中文原文", k, l);
            }
        }
    }
}
