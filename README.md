# 扫描件转 Word（Rust 版）

把**扫描版 / 拍照版 PDF**（没有文字层、复制不出字的那种）转换成**保留版式、可直接编辑**的 `.docx`，
也能只把里面的**表格导成 `.xlsx`**。

全部处理在本机完成，**不联网、不上传、不调用任何云端 API**。
界面有 7 种语言，识别语言除内置的中英日拉丁外还能按需加语言包（韩/俄/阿/泰/印地），
详见[多语言](#多语言)。

这是 [pdfRec](https://github.com/cobraliu/pdfRec)（Python 版）的 Rust 重写：**判据、阈值、输出格式逐条照搬**，
同一份 PDF 出来的 Word 结构一致。换栈只为解决两件事——**体积**和**授权**。

| | Python 版 | 本项目 |
|---|---|---|
| 下载体积 | 178 MB (dmg) | **51.6 MB** (dmg) |
| 安装后占用 | 401 MB | **84 MB**（其中 32 MB 是模型） |
| 程序本体 | 370 MB（Python 运行时 + PySide6 + OpenCV + NumPy…） | **27 MB**（含静态链接的 ONNX Runtime） |
| 96 页合同耗时 | 162 s | **105 s** |
| PDF 渲染 | PyMuPDF — **AGPL-3.0** | pdfium — **BSD**，闭源商用无需授权 |
| 启动到出第一页 | 数秒（解压自身 + 导入） | 秒级 |

体积和耗时都在同一台 M 系列 Mac 上实测（96 页扫描合同，`--to both`）。

---

## 它做了什么

```
                      ┌─ 有文字层 ─> 直接取字 ────┐
                      │   (原生 PDF)   pdfium     │      ┌─> Word  手写 OOXML   (默认, 保版式)
PDF ──渲染──> 页图 ──┤                           ├─版面重建─┤
    pdfium            └─ 没文字层 ─> OCR ────────┘  规则引擎  └─> Excel rust_xlsxwriter (只导表格)
                          (扫描件)  PP-OCRv6 ONNX    + 框线
```

拿到「这一行字在哪个坐标」之后，把它还原成有结构的 Word 才是难点。版面重建做了这些：

| 处理 | 说明 |
|---|---|
| **原生 PDF 不走识别** | Word / Excel 直接导出的 PDF 自带文字层，每个字是什么、在哪儿本来就精确写着——**直接取，不识别**。识别占九成时间，这一步把它整段省掉，而且不会认错字。实测一份 Excel 导出的报表，72 格里 OCR 错了 6 格（漏字、整句截断、两列串成读不通的乱码），走文字层零错。判据是本页可见字符数：扫描件常被塞进页码、水印一类的文字层残渣，太少就当没有。相邻两格的字常常各自把格宽填满、中间一丝空隙都没有（表头尤其如此），这时靠间距切不开，**改用 PDF 里画着的那些竖线当单元格边界**——原生 PDF 的表格线是矢量图形，坐标精确写在文件里，不必去图上找 |
| **框线表格还原** | 原件画了框线的表格**照框线还原行列**：缺哪条内部框线，那里在原件里就是一个**合并单元格**，还原出来照样是合并的（Word 里真合并，Excel 里 `merge_range`）。一格里排了三行字也还是一格——按文字间距猜列时它必然被拆成三行，这是扫描件转表格最常见的坏结果。框线用**游程编码的一维形态学开运算**取，厚度按「面积÷长度」判，不看外接框：扫描件总带零点几度歪斜，一条 1500 px 的横线外接框就有七八像素高。汉字竖笔跟框线一样细一样直，靠「身上压过两条以上横线」+「不落在 OCR 文字框内部」两道判据滤掉 |
| **框线照原件逐边画** | 不是每张表都四面围严。签章表常常只有外框没有内线，报价单里的三线表只画上下两条，还有整张表不画左右边框的——**每一格的四条边分别照原件画**，不是「要么全画要么全不画」。外框没画时，表宽按横线画到哪儿补回来，否则最外那一列连字带线都会掉在表外 |
| **多列表格还原** | 无框线的多列版面（`序号 \| 数量 \| 描述 \| 备注`、技术规格的「标签—值」两列）还原成**无边框表格**。列位按各格左边界聚类得出，不靠找空白通道——扫描件里相邻两列常常只差 1.5% 页宽，通道法会把它们并掉 |
| **表头与跨页续表** | 首行全是短标签就认作表头：加粗并标记 `w:tblHeader`，表格跨页时 Word 自动重复。下一页开头若列数列位都对得上，直接续到上一张表里 |
| **格内折行归位** | 单元格里的折行（`supplied by the` / `customer`）并回同一格；散文行（`需方：XX ␣␣ 签订时间：YY`）与表格列位对不上，会被挡在表格区外 |
| **数学分式** | 上下叠排的分式还原成**真正的 Word 公式**（OMML），双击可进公式编辑器。分数线靠页图像素判定——峰值墨迹覆盖 >60%、厚度 ≤5 px、上下近乎全白，密排汉字行和表格框线都不会误判 |
| **续行合并** | 被 OCR 拆断的长段落重新接回一段。判据是「上一行是否排到右边界」——没排满的行绝不吸收下一行，小标题不会被吞进正文 |
| **项目符号找回** | OCR 会直接丢弃 `•` 这类图形符号，靠「缩进超出正文基准」把列表项还原出来 |
| **噪声剔除** | 骑缝章、手写签名常被识别成乱码，按「短字符串 + 低置信度」剔除。左右页边被裁掉的竖排印记碎片不看置信度直接剔——它们能识别到 0.99，却会横跨上下两行把两行粘成一行 |
| **横版原件** | 原件多数页是横版时，输出的 Word 自动用横向页面 |
| **导出 Excel** | 同一套版面判定另出一份 `.xlsx`：**只导表格**，每张表上方一行灰色「原第 N 页」标注，**跨整张表宽度合并居中**。跨页续表接成一张连续矩形、标注写成「原第 53–56 页」——中间夹一行页码标记，筛选和透视就废了。纯数字单元格存成真数字（小数位用数字格式保住），编号型号这类长串数字留文本，免得掉前导零或变成科学计数法 |

---

## 使用

### 方式一：下载现成程序

到 [Releases](../../releases) 下载对应平台的包：

| 平台 | 下载 | 说明 |
|---|---|---|
| Windows x64 | `pdf2doc-windows-x64.zip` | 解压后双击 `pdf2doc-gui.exe` |
| macOS (Apple 芯片) | `pdf2doc-macos-arm64.dmg` | 打开后把「扫描件转Word」拖进 Applications；首次启动如被拦，**右键 → 打开** |
| Linux x64 | `pdf2doc-linux-x64.tar.gz` | 解压后 `./pdf2doc-gui` |

包里是**一个目录**：两个可执行文件（图形界面版 `pdf2doc-gui` / 命令行版 `pdf2doc`）、
一个 `libpdfium`、一个 `models/`。三者必须放在一起——程序按可执行文件的相对位置找它们。

> **Intel Mac 没有预编译包**：`ort` 不提供 `x86_64-apple-darwin` 的 ONNX Runtime 静态库，
> 得自行编译 onnxruntime 后用 `ort` 的 `load-dynamic` 特性链接。

界面：把 PDF（或整个文件夹）拖进左侧列表 → 点「开始转换」。

输出格式可以**逐个文件设**：队列里每一行右侧有个下拉框，默认是「默认·Word」——
跟着顶栏那个走，改顶栏就一起改；单独选了 Word / Excel / 两份的那几行则不受影响。
一队文件里有的要表格有的要正文时，不用分两趟跑。

### 方式二：从源码构建

需要 Rust 1.82+：

```bash
git clone <repo> && cd ScannedPdf2doc

# 1. 取 pdfium 动态库(BSD), 放进 vendor/
curl -fsSL -o pdfium.tgz \
  https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-mac-arm64.tgz
mkdir -p vendor tmp && tar -xzf pdfium.tgz -C tmp && cp tmp/lib/libpdfium.dylib vendor/

# 2. 取 OCR 模型(32 MB), 放进 models/
#    源是 ModelScope(魔搭, 阿里云), 国内直连。链接和校验值都抄自 RapidOCR 的
#    default_models.yaml, 拉下来与 Python 版用的是同一批文件(校验值一致)
mkdir -p models
B=https://www.modelscope.cn/models/RapidAI/RapidOCR/resolve/v3.9.2/onnx
curl -fL -o models/PP-OCRv6_det_small.onnx  $B/PP-OCRv6/det/PP-OCRv6_det_small.onnx
curl -fL -o models/PP-OCRv6_rec_small.onnx  $B/PP-OCRv6/rec/PP-OCRv6_rec_small.onnx
curl -fL -o models/ch_ppocr_mobile_v2.0_cls_mobile.onnx \
  $B/PP-OCRv4/cls/ch_ppocr_mobile_v2.0_cls_mobile.onnx
(cd models && shasum -a 256 -c <<'EOF'
090f04abcd9d9a7498bc4ebf677e4cb9bdce1fe4197ddb7e529f1ef44e1ff94f  PP-OCRv6_det_small.onnx
6f327246b50388f3c176ae304bd95767ea6dc0c9ae92153ef8cbe210b3c14884  PP-OCRv6_rec_small.onnx
e47acedf663230f8863ff1ab0e64dd2d82b838fceb5957146dab185a89d6215c  ch_ppocr_mobile_v2.0_cls_mobile.onnx
EOF
)
# 也可以让 RapidOCR 代劳(它去的是同样这三个地址): pip install "rapidocr>=3.0" onnxruntime
# && python -c "from rapidocr import RapidOCR; RapidOCR()", 然后把它 models/ 里的 *.onnx 拷过来

# 3. 编译
cargo build --release
./target/release/pdf2doc examples/sample_scanned.pdf --to both
```

Windows / Linux 把上面的 `pdfium-mac-arm64.tgz` 换成 `pdfium-win-x64.tgz` / `pdfium-linux-x64.tgz`
（库文件分别是 `bin/pdfium.dll`、`lib/libpdfium.so`）。

只要命令行版可以 `cargo build --release --no-default-features --bin pdf2doc`，省掉整套 GUI 依赖。

> **构建时要联的网**（成品运行时一次都不联）：OCR 模型走 ModelScope，国内直连；
> pdfium 走 GitHub Releases、`ort` 的 ONNX Runtime 预编译库走 `cdn.pyke.io`，
> 这两处国内可能要挂代理或换镜像。crates.io 可以换 `rsproxy.cn` 之类的镜像。
> 断网机器上怎么编，见下一节。

### 完全离线

**文档处理这条路上一次网都不联**：整个运行期依赖树里没有任何网络库（`cargo tree -e normal`
里搜不到 rustls/reqwest/hyper/ureq，`otool -L` 只剩系统框架），渲染、OCR、写文件全在本地。
`ort` 那条 TLS 只在编译期用来下 onnxruntime，不进成品。

唯一会出网的动作是**下载语言包**（见[多语言](#多语言)）——只在你主动挑一种内置之外的识别语言
时发生，走的是外部 `curl`，不是链进程序里的网络库。不碰这个开关，程序连一个 socket 都不开；
文档内容任何时候都不出本机。

**最小可运行文件集**（macOS arm64 实测，换到别的目录也能跑）：

| 文件 | 大小 |
|---|---|
| `pdf2doc`（命令行版） | 20 MB |
| `libpdfium.dylib` | 6.9 MB |
| `models/PP-OCRv6_det_small.onnx` | 9.5 MB |
| `models/PP-OCRv6_rec_small.onnx` | 20 MB |
| `models/ch_ppocr_mobile_v2.0_cls_mobile.onnx` | 0.6 MB |
| **合计** | **57 MB** |

三个模型缺一不可（检测、识别、方向分类各一个）。库和 `models/` 默认按**可执行文件**的
相对位置找：同目录的 `libpdfium.*` 与 `models/`，`.app` 里则是 `Contents/Frameworks`
和 `Contents/Resources/models`；也可以用 `PDFIUM_LIB`、`PDF2DOC_MODELS` 指到别处。
图形界面版是 27 MB，**两个可执行文件不必都留**——只要界面就删掉 `pdf2doc`。

**在断网机器上编译**：在一台能联网、且同 OS 同架构的机器上备好三样拷过去——

1. crates 依赖：`cargo vendor`，按它打印的内容写进 `.cargo/config.toml`
2. ONNX Runtime 静态库：`libonnxruntime.a`（80 MB）。联网构建过一次的机器上它在
   `~/Library/Caches/ort.pyke.io/dfbin/<target>/<hash>/`（Linux 是 `~/.cache/…`，
   Windows 是 `%LOCALAPPDATA%\…`），拷走整个目录即可
3. `libpdfium.*` 和三个 `.onnx`

断网那台上：

```bash
export ORT_LIB_LOCATION=/路径/到/含-libonnxruntime.a-的目录
CARGO_NET_OFFLINE=true cargo build --release
```

> `ORT_LIB_LOCATION` 不能省。只设 `CARGO_NET_OFFLINE`（或 `--offline`）的话，`ort`
> 的构建脚本会认为"不许下载"就干脆什么都不链，最后在链接期报一屏 `symbol(s) not found`
> ——即使本地缓存里那份 `.a` 就在那儿也不会用。

### 命令行

```bash
pdf2doc 文件.pdf                      # 出 docx
pdf2doc 文件.pdf --to xlsx            # 只导表格到 Excel
pdf2doc a.pdf b.pdf --to both -o 输出目录
pdf2doc 文件.pdf --long-edge 3200     # 提高渲染精度(小字识别不准时)
pdf2doc 文件.pdf --no-grid            # 关掉按框线还原
pdf2doc 文件.pdf --no-text-layer      # 不用 PDF 自带的文字层, 一律识别
pdf2doc 文件.pdf --lang en            # 界面/日志/产物里的标记用英文
pdf2doc 韩文件.pdf --ocr-lang ko      # 按韩文识别(缺语言包时先下)
```

`--to` 对本次命令里的所有文件一起生效；要给单个文件另设格式，用图形界面，或者按格式分两条命令跑
（识别结果有缓存，第二条不会重新 OCR）。

识别结果缓存在 PDF 同目录的 `.pdf2doc_cache/`，调参数重跑时不必重新 OCR。

### 界面上的几条规矩

按 Swiss / 极简那一路做的：不加装饰，靠留白和字号分层级，**整屏只有一处填色**——「开始转换」
（转起来之后变成红色的「停止」）。其余按钮长得一样重，那颗才是这屏的目的。

- **状态同时用形状和颜色**：`○ 等待 / ▶ 转换中 / ✓ 成了 / ✗ 砸了`。只靠红绿分的话，
  红绿色觉障碍看到的是四行一模一样的点。
- **颜色对着背景算过对比度**，深浅两套主题各一份，最低的一档 5.3:1（WCAG AA 要 4.5:1）。
  原来那组值有四个在 3.4~4.2 之间——看得见，但看不清。egui 跟随系统主题，深色下那组浅色值
  会低到 2:1 左右，所以不能只留一套。
- **「清空」可以撤销**，不弹确认框。队列可能是从三个文件夹里一个个挑出来的，
  与其拦住那 99 次真想清空的，不如让按错的那次能收回来。单行的 `×` 同样可撤。
- **失败的行有一颗 `↻`**：换个参数只重转这一个，不必把整队重跑（识别结果有缓存，重转很快）。
- **失败原因直接摊在行下面**，不藏在悬停提示里；转好的 `✓` 可以点开产物，鼠标移上去变成手型。

**从不覆盖已有文件**：输出目录里已经有 `合同.docx` 了，这次就写成 `合同-20260813234726.docx`
（本地时间 `yyyymmddHHMMSS`），日志里会说明。转出来的 Word 通常还要人接着改——补签章、调格式、
改错字——换个参数重跑一次就把一下午的修订盖没了，而且程序是直接覆写，没有回收站可捞。
落盘用的是 `create_new`，挑名和写入之间就算被塞进个同名文件也只会报错，不会盖。

### 多语言

**界面语言**和**识别语言**是两件独立的事，各有各的选单。一个韩国用户完全可能把界面设成韩语，
却整天扫中文合同；反过来也一样。

**界面语言**：简体中文 / 繁體中文 / English / Español / Deutsch / 日本語 / 한국어。
默认跟系统走（`LC_ALL` → `LC_MESSAGES` → `LANG` → `LANGUAGE`，macOS 上还会问一次
`AppleLocale`，Windows 上问 `GetUserDefaultLocaleName`），认不出的语种落到英文。
命令行用 `--lang`，图形界面在顶栏右上角改，改完记在
`<配置目录>/scannedpdf2doc/settings.json`（macOS 是 `~/Library/Application Support`）。

它管的不只是按钮上的字：**转出来的 Word / Excel 里那些标记也跟着走**——「原第 N 页」、
Excel 的工作表名、单页失败的占位行。这些字会跟着文件一路传下去，所以宁可跟界面语言走，
谁转的文件谁最可能第一个读它。

**识别语言**：内置的那个 rec 模型认**简繁汉字 + 日文假名 + 拉丁（含重音）+ 希腊**，
中英混排、中日混排都在它一个模型里，不用切。它认不了的另开语言包：

| `--ocr-lang` | 认什么 | 体积 |
|---|---|---|
| （不给） | 中文简繁 · 英文 · 日文假名 · 拉丁 · 希腊 | 内置 |
| `ko` | 韩文谚文 | 13 MB |
| `ru` | 西里尔（俄/乌/塞…） | 7.7 MB |
| `ar` | 阿拉伯文 | 7.7 MB |
| `th` | 泰文 | 7.5 MB |
| `hi` | 天城文（印地/马拉地…） | 7.6 MB |
| `ja` | 日文（假名+汉字专训，扫日文原件比内置准） | 9.3 MB |

一个语言包就是**一个 rec `.onnx`**——字符集嵌在模型的 ONNX metadata 里，
检测和方向分类那两个模型与语种无关，不用换。所以**没法混**：CTC 识别头的输出维度就是
一个字符集，选了韩文那一份，同一页上的汉字就认不出来。

缺哪个现下哪个，源是 ModelScope（RapidAI/RapidOCR，国内直连），下完按 SHA-256 核对，
存在 `<缓存目录>/scannedpdf2doc/packs/`（macOS 是 `~/Library/Caches`）——放缓存里是因为
这些东西随时能重新下回来，用户清一次缓存不该把界面语言这种明确设过的选择也清掉。
下载先落 `.part` 再改名，中途关掉不会留下一个大小对不上的模型文件。

下载走的是外部 `curl`，不是链进来的 HTTP 客户端：为一个偶尔按一次的按钮拖进一整套 TLS 栈
（rustls → ring，那里面有 OpenSSL 血统的汇编）不划算，[授权那条线](#依赖与授权)也不想为它
多一份要交代的东西。没有 `curl` 的机器会明说「这台机器上没有 curl」，而不是静悄悄失败。

**下不成就不切**是这里最要紧的一条。切过去而包是空的，下一次转换会拿中文模型去认韩文，
出来的不是「没结果」，是一整页**看着像结果的错字**。

---

## 示例

`examples/sample_scanned.pdf` 是一份**合成的**扫描件（内容全部虚构，与任何真实合同无关）：
纯图像、无文字层的三页 A4，带轻微歪斜与噪点，覆盖中英对照、`1.` / `1.1` / `三、` 编号层级、
「标签—值」多列版面、项目符号列表、页眉页脚与印章签名，第 3 页是一张**画了框线的供货清单**：
一格里排两行字、「备注」跨 3 行合并、「合计」跨 4 列合并。

```bash
pdf2doc examples/sample_scanned.pdf --to both
```

这份示例的输出与 Python 版**逐段逐格一致**（差异只有 3 处 OCR 字符，均为标点全半角）。

---

## 和 Python 版的一致性是怎么验的

拿一份 **96 页真实扫描合同**两边各跑一遍，按文档里的「原第 N 页」标记逐页对齐比结构：

| 指标 | 结果 |
|---|---|
| 版面结构完全一致的页 | **71 / 96** |
| 表格总数 | **113 = 113** |
| Excel 里的页码标记序列（含 4 处跨页续表「原第 27–28 页」） | **完全一致** |
| OCR item 逐字符相同 | 92.4%（5249 / 5679） |
| 检测框总数 | 5708 (Python) vs 5712 (Rust) |

结构不一致的 25 页，用**判据归因法**查清了责任方：把 Rust 的检测框喂给 **Python 自己的版面算法**，
若得出的结果与 Rust 一致，说明差异出在 OCR 而非移植。25 页**全部**如此——版面重建这一层已经对齐，
剩下的是识别层面的字符噪声（两边都有认错的行，只是认错的位置不同）。

这套对比过程揪出了三个移植 bug，都已修：

1. `clean()` 把单个 `\n` 也压成了空格 —— Python 的 `re.sub(r'\s{2,}', ' ', t)` 只压 2 个以上的空白。
   单元格内的换行是有意义的（「手动下单/自动下单」是两条并列的值），压掉就粘成一行了。
2. 正文块把表格状态一起清了 —— Python 只清框线表状态。结果**所有跨页续表都接不上**，
   一张跨 4 页的长表被切成 4 张。
3. 页眉正则里用了行末反斜杠折行，而 Rust 的**裸字符串** `r"..."` 不处理转义 ——
   `\` + 换行被当成"转义的换行符"粘在下一个分支前面，那一支永远匹配不上，
   于是那个页眉每页都漏进正文，还把它后面的表格挤得不再是本页第一个块，连带毁掉续表判定。

---

## 参数

| 参数 | 默认 | 什么时候调 |
|---|---|---|
| `--long-edge` | 2560 | 小字/密排识别不准 → 调高；追求速度 → 调低。实际 dpi 按页面尺寸倒推并夹在 150~300，所以 A3 图纸和 A5 单据出来的字号一样大 |
| 续行判定 `full_line` | 0.78 | 段落被错误粘成一坨 → 调高；本该连起来的句子被拆断 → 调低 |
| 列间空白 `gutter` | 0.035 | 多列版面没被识别成表格 → 调低；正文词间距被当成列 → 调高 |
| 项目符号缩进 `bullet_ind` | 0.030 | 列表项没被识别 → 调低；正文被误判成列表 → 调高 |
| 印章置信度 `stamp_conf` | 0.88 | 印章乱码混进正文 → 调高；正常短词被误删 → 调低 |

图形界面里点「高级」可以直接调；命令行目前只暴露了 `--long-edge` 和几个开关。

### 当库用：内存开关

嵌到内存吃紧的环境（手机是典型）时，`ocr::Engine::load_with` 收一个 `EngineOptions`：

```rust
use scannedpdf2doc::ocr::{Engine, EngineOptions};

// 峰值 695 -> 567 MB, 识别慢 0.02s, 识别结果逐字不变
let engine = Engine::load_with(&model_dir, EngineOptions::low_memory())?;
```

`low_memory()` 只开 `lazy`——让检测、方向分类、识别三个 session 轮流上场、用完就放。
`run()` 本来就是一段段做完的，谁也不需要跟别人同时在场，所以这么改不影响结果。

峰值内存几乎全花在**检测**那一次推理上（拿纯白页测，跑完检测就返回，已经占到
570 MB），而它随检测输入的面积走。所以 `det_max_side` 是唯一还能大幅往下压的
开关——但它**会改结果**，默认不开，要用请先拿自己的样本验一轮。

另外两个容易想当然的错觉，实测都不成立：降线程数几乎不省内存（10→2 只省 1 MB，
却慢 1.7 倍）；关掉 ORT 的 arena 在 `lazy` 下反而更费（每块单独 malloc，归还
不及时，RSS 高水位更高）。

---

## 换栈都换掉了什么

Python 版一半体积来自那些为了几个函数而整包引入的库。重写时它们各自的用途都用几十行代码顶掉了：

| Python 依赖 | 装机体积 | Rust 里怎么办的 |
|---|---|---|
| PyMuPDF | 46 MB | pdfium（7 MB），顺带甩掉 AGPL |
| OpenCV | 87 MB | 只用到形态学开运算和连通域 → **游程编码的一维开运算** + 迭代式 flood fill，`src/imgutil.rs` |
| NumPy + SciPy | 60 MB | `ndarray`，只在推理张量上用 |
| pyclipper | — | 检测框外扩 → **按边偏移后求交点**，`src/geom.rs` |
| shapely | — | 最小外接矩形 → **Andrew 单调链凸包 + 旋转卡壳**，同上 |
| python-docx / openpyxl | 12 MB | 手写 OOXML / `rust_xlsxwriter` |
| PySide6 | 120 MB | `egui`（约 8 MB，含在可执行文件里） |
| Python 运行时 | 45 MB | 无 |

ONNX Runtime 是唯一没法省的（**静态链接进可执行文件**，不再是单独的 dylib），
模型的 32 MB 也一分不能少——识别精度直接挂在上面。

内存上还顺手改了个结构：Python 版把每页 PNG 落盘来控内存，这里改成
**逐页流式处理**（渲染 → 识别 → 重建 → 写进输出对象，页图随即释放），不落盘也不占内存。
96 页的合同全渲染成 300 dpi 灰度图是 800 MB，流式处理下峰值只有一页。

---

## 已知限制

- **签名、印章、二维码、页眉页脚会被剔除**（默认行为，可关闭）。转出的 Word 若要作为正式文件，签章需另行处理。
- **原文的下划线、字符级格式不还原**，只保留标题加粗与段落结构。
- **图纸、示意图不会带过来**，只提取其上的文字。
- **公式只还原一层分式**：根号、积分、求和、上下标不识别。
- **Excel 输出只有表格**：正文段落、标题、项目符号一概不进 `.xlsx`。分式在 Excel 里摊平成 `(a-b)/a` 这样的文本。
- **框线还原受制于原件真有框线**：没画线的相邻表头会并成一格。既然是照原件逐边画，
  原件上那条线淡到检不出来，转出来的表那条边就是空的。
- **窄列里的小字可能串格**：OCR 的行检测会把相邻两个窄格的字连成一行。
- OCR 在型号、公差这类**密集数字**上仍可能出错，重要文件建议人工抽查。
- **扫描件被别的工具塞过文字层时，好坏分不出来**：程序按「有就用」处理，那层要是当初就认错了，
  这里照样错。信不过就 `--no-text-layer`（界面上是「有文字层就直接用」那个勾）自己重认一遍。
- 图形界面**不内置字体**（那会让包大好几 MB），启动时从系统里找一个 CJK 字体挂上去；
  界面设成韩语时另外找一个谚文字体（汉字字体里一个谚文都没有，而队列里的文件名又可能是中文，
  所以那种情况下两批都挂）。极简 Linux 环境里若一个都没有，界面会显示成方框，
  装 `fonts-noto-cjk` 即可。
- **一次只能按一种语言识别**：CTC 识别头的输出维度就是一个字符集。内置那份已经把
  中英日拉希腊放在一起了，但韩、俄、阿、泰、印地各自独立，同一页上混排认不了。

---

## 开发

推之前本地先跑一遍，CI 卡的就是这三条，一条不过就判红：

```bash
cargo fmt --check                                      # 格式, 用 rustfmt 默认配置
cargo clippy --release --all-targets -- -D warnings    # lint, 零容忍
cargo test --release                                   # 单元 + 回归
```

`tests/rebind.rs` 盯的是「同一个进程里建第二个 `Renderer`」——pdfium 的绑定是进程级全局的，
第二次绑定必然报 `AlreadyInitialized`，图形界面转完一批再转一批就会踩到。它要 `vendor/`
里有 libpdfium 才真跑，找不到会自己跳过而不是判红。

两条 workflow 各管一段：

| 文件 | 触发 | 干什么 |
|---|---|---|
| `.github/workflows/ci.yml` | 推 main / 提 PR | Linux 单平台：上面三条 + 拿 `examples/sample_scanned.pdf` 跑一遍真转换 |
| `.github/workflows/build.yml` | 打 `v*` tag | Windows / macOS / Linux 三个发行包，附 License，建 Release |

模型和 pdfium 不进仓库，两条 workflow 都是现拉的：pdfium 取 bblanchon 的最新预编译包，
模型让 RapidOCR 自己去 ModelScope 拿默认那三个——和 Python 版用的是同一批文件。

---

## 依赖与授权

| 组件 | 用途 | 授权 |
|---|---|---|
| pdfium | PDF 渲染 | BSD-3-Clause |
| ONNX Runtime（经 `ort`） | 模型推理 | MIT |
| PP-OCRv6 (ONNX) | 文字检测/识别/方向分类，共 32 MB | Apache-2.0 |
| PP-OCRv4/v5 rec (ONNX) | 韩/俄/阿/泰/印地/日 语言包，按需下载 | Apache-2.0 |
| `sha2` | 校验下下来的语言包 | MIT / Apache-2.0 |
| `image` / `ndarray` / `zip` | 图像解码、张量、docx 打包 | MIT / Apache-2.0 |
| `rust_xlsxwriter` | 生成 xlsx | MIT / Apache-2.0 |
| `egui` / `eframe` | 图形界面 | MIT / Apache-2.0 |

**整条链上没有 copyleft**——Python 版最大的授权障碍 PyMuPDF（AGPL-3.0）已被 pdfium 替掉，
可以闭源分发或商用。本项目自身为 Apache-2.0，见 `LICENSE`。
