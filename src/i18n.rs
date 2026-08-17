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
