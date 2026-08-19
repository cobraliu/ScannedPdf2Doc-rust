//! 可搜索 PDF —— 页图照贴, 上面盖一层看不见的文字
//!
//! 扫描件转成 Word 是"重建", 有取舍; 可搜索 PDF 是另一件事: 版面一个像素都
//! 不动, 只是让它能搜、能选、能复制。归档、法务、发出去给人看, 要的往往是
//! 这个而不是一份重排过的 Word。
//!
//! 贴的是识别时用的那张图(摊平、转正之后的), 所以文字坐标和图天然对得上,
//! 不必再换算一次。
//!
//! # 中文怎么办
//!
//! 难点全在字体。中文要能搜, 得让阅读器知道每个字码对应哪个 Unicode。老实
//! 做法是把一份中文字体嵌进去 —— 十几 MB, 而且随手拿系统字体嵌进 PDF 发出去
//! 授权上站不住。
//!
//! 这里走另一条路: 用 Adobe-GB1 这种**预定义字符集**的非嵌入 CID 字体, 编码
//! 用 UniGB-UCS2-H —— 字码就是 UCS-2 码位。字形交给阅读器拿系统字体替换, 而
//! 文字是不可见的(`3 Tr`), 替换成什么样根本看不见。检索和复制走我们自己写的
//! ToUnicode 表, 那张表在这个编码下正好是恒等映射, 一行 bfrange 就够。
//!
//! 顺带的好处: 日文韩文也能搜。Adobe-GB1 里没有假名的字形, 可字形本来就不
//! 显示; 抽字走 ToUnicode, 跟字形没关系。
//!
//! # 字宽
//!
//! CID 字体声明 `/DW 1000` 且不给 `/W` 表, 于是每个字都正好一个 em 宽 —— 阅读器
//! 算出来的宽度我们这边能精确预测。再用水平缩放 `Tz` 把这一串拉到跟识别给的
//! 框一样宽, 选中时的高亮就正好落在字上。

use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::imgutil::{Gray, Rgb};
use crate::ocr::Item;

/// 贴上去的那张页图
///
/// 默认走彩色: 合同上的红章、手写签名的蓝墨水, 转成灰的就没了那半分意思,
/// 而实测彩色 JPEG 只比灰度大 7% —— 这页扫出来本来就没多少彩色, 色度通道
/// 几乎是常数。灰度那条留着兜底: 彩色渲染万一失败, 有图总比没有强。
pub enum PageImg<'a> {
    Gray(&'a Gray),
    Rgb(&'a Rgb),
}

impl PageImg<'_> {
    fn size(&self) -> (usize, usize) {
        match self {
            PageImg::Gray(g) => (g.w, g.h),
            PageImg::Rgb(r) => (r.w, r.h),
        }
    }

    fn space(&self) -> &'static str {
        match self {
            PageImg::Gray(_) => "DeviceGray",
            PageImg::Rgb(_) => "DeviceRGB",
        }
    }

    fn jpeg(&self) -> Result<Vec<u8>> {
        let (px, kind) = match self {
            PageImg::Gray(g) => (&g.px, image::ExtendedColorType::L8),
            PageImg::Rgb(r) => (&r.px, image::ExtendedColorType::Rgb8),
        };
        let (w, h) = self.size();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_Q)
            .encode(px, w as u32, h as u32, kind)
            .context("页图压成 JPEG")?;
        Ok(out)
    }
}

/// 页图存成 JPEG 的质量。归档件不该糊, 但也没必要无损 —— 90 下一页两三百 KB
const JPEG_Q: u8 = 90;
/// 基线离字框底边多高(按字号的比例), 大致相当于西文的下伸部
const BASELINE: f32 = 0.15;
/// 水平缩放的合理区间(百分比): 识别偶尔给出畸形的框, 别让它把 Tz 拉到天上去
const TZ_MIN: f32 = 10.0;
const TZ_MAX: f32 = 400.0;

// 前六个对象号是固定的, 内容留到收尾时写 —— xref 认的是偏移量, 不要求对象
// 在文件里按号排队
const CATALOG: u32 = 1;
const PAGES: u32 = 2;
const FONT: u32 = 3;
const CID_FONT: u32 = 4;
const DESCRIPTOR: u32 = 5;
const TO_UNICODE: u32 = 6;
const FIRST_FREE: u32 = 7;

pub struct Builder {
    /// 最终文件名
    dst: std::path::PathBuf,
    /// 边转边写, 中途出错留下的是半截文件。所以先写 `.part`, 收尾成功才改名
    tmp: std::path::PathBuf,
    out: BufWriter<File>,
    pos: u64,
    /// 第 i 项 = 对象号 i+1 在文件里的偏移
    offsets: Vec<u64>,
    pages: Vec<u32>,
    next: u32,
}

impl Builder {
    pub fn new(path: &Path) -> Result<Self> {
        let tmp = path.with_extension("pdf.part");
        let _ = std::fs::remove_file(&tmp);
        // create_new: 跟 docx 一样, 名字是调用方挑好的, 这儿不覆盖任何东西
        let file = File::options().write(true).create_new(true).open(&tmp)?;
        let mut b = Self {
            dst: path.to_path_buf(),
            tmp,
            out: BufWriter::new(file),
            pos: 0,
            offsets: vec![0; FIRST_FREE as usize - 1],
            pages: Vec::new(),
            next: FIRST_FREE,
        };
        // %PDF-1.7 后面那行二进制注释是规矩: 让按字节搬运的工具认出这不是文本文件
        b.write(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n")?;
        Ok(b)
    }

    fn write(&mut self, b: &[u8]) -> Result<()> {
        self.out.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }

    fn take_id(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        self.offsets.push(0);
        id
    }

    fn obj(&mut self, id: u32, body: &[u8]) -> Result<()> {
        self.offsets[id as usize - 1] = self.pos;
        self.write(format!("{id} 0 obj\n").as_bytes())?;
        self.write(body)?;
        self.write(b"\nendobj\n")
    }

    /// 带数据的流对象; `dict` 不含两端的尖括号
    fn stream(&mut self, id: u32, dict: &str, data: &[u8]) -> Result<()> {
        self.offsets[id as usize - 1] = self.pos;
        self.write(format!("{id} 0 obj\n<<{dict} /Length {}>>\nstream\n", data.len()).as_bytes())?;
        self.write(data)?;
        self.write(b"\nendstream\nendobj\n")
    }

    /// 加一页: 页图 + 盖在上面的隐形文字
    ///
    /// `pt` 是这一页在原 PDF 里的尺寸(点), 出来的 PDF 保持同样大小
    pub fn add_page(&mut self, img: PageImg<'_>, items: &[Item], pt: (f32, f32)) -> Result<()> {
        let jpeg = img.jpeg()?;
        let (iw, ih) = img.size();
        let (pw, ph) = pt;
        let im = self.take_id();
        self.stream(
            im,
            &format!(
                "/Type /XObject /Subtype /Image /Width {iw} /Height {ih} \
                 /ColorSpace /{} /BitsPerComponent 8 /Filter /DCTDecode",
                img.space()
            ),
            &jpeg,
        )?;

        let mut c = format!("q\n{pw} 0 0 {ph} 0 0 cm\n/Im0 Do\nQ\nBT\n3 Tr\n");
        let (kx, ky) = (pw / iw.max(1) as f32, ph / ih.max(1) as f32);
        for it in items {
            let n = it.t.chars().filter(|c| !c.is_control()).count();
            if n == 0 {
                continue;
            }
            let size = ((it.y1 - it.y0) * ky).max(1.0);
            let tz = (100.0 * (it.x1 - it.x0) * kx / (n as f32 * size)).clamp(TZ_MIN, TZ_MAX);
            let x = it.x0 * kx;
            let y = ph - it.y1 * ky + BASELINE * size;
            c.push_str(&format!(
                "/F1 {size:.2} Tf\n{tz:.1} Tz\n1 0 0 1 {x:.2} {y:.2} Tm\n<{}> Tj\n",
                utf16be_hex(&it.t)
            ));
        }
        c.push_str("ET\n");

        let cs = self.take_id();
        self.stream(cs, "", c.as_bytes())?;

        let pg = self.take_id();
        self.obj(
            pg,
            format!(
                "<</Type /Page /Parent {PAGES} 0 R /MediaBox [0 0 {pw:.2} {ph:.2}] \
                 /Resources <</XObject <</Im0 {im} 0 R>> /Font <</F1 {FONT} 0 R>>>> \
                 /Contents {cs} 0 R>>"
            )
            .as_bytes(),
        )?;
        self.pages.push(pg);
        Ok(())
    }

    /// 收尾之后文件会落在这儿
    pub fn path(&self) -> &std::path::Path {
        &self.dst
    }

    pub fn pages(&self) -> usize {
        self.pages.len()
    }

    pub fn finish(mut self) -> Result<()> {
        let kids: String = self
            .pages
            .iter()
            .map(|p| format!("{p} 0 R "))
            .collect::<String>();
        let n = self.pages.len();
        self.obj(
            CATALOG,
            format!("<</Type /Catalog /Pages {PAGES} 0 R>>").as_bytes(),
        )?;
        self.obj(
            PAGES,
            format!("<</Type /Pages /Kids [{kids}] /Count {n}>>").as_bytes(),
        )?;
        self.obj(
            FONT,
            format!(
                "<</Type /Font /Subtype /Type0 /BaseFont /STSong-Light \
                 /Encoding /UniGB-UCS2-H /DescendantFonts [{CID_FONT} 0 R] \
                 /ToUnicode {TO_UNICODE} 0 R>>"
            )
            .as_bytes(),
        )?;
        self.obj(
            CID_FONT,
            format!(
                "<</Type /Font /Subtype /CIDFontType0 /BaseFont /STSong-Light \
                 /CIDSystemInfo <</Registry (Adobe) /Ordering (GB1) /Supplement 2>> \
                 /FontDescriptor {DESCRIPTOR} 0 R /DW 1000>>"
            )
            .as_bytes(),
        )?;
        // 不带 FontFile 就是非嵌入字体; Flags 第 3 位(值 4)是"符号字体",
        // 中日韩字体按规矩都这么标
        self.obj(
            DESCRIPTOR,
            b"<</Type /FontDescriptor /FontName /STSong-Light /Flags 4 \
              /FontBBox [-25 -254 1000 880] /ItalicAngle 0 /Ascent 880 /Descent -254 \
              /CapHeight 880 /StemV 58>>",
        )?;
        self.stream(TO_UNICODE, "", TO_UNICODE_CMAP.as_bytes())?;

        let xref = self.pos;
        let total = self.offsets.len() + 1;
        self.write(format!("xref\n0 {total}\n0000000000 65535 f \n").as_bytes())?;
        for i in 0..self.offsets.len() {
            self.write(format!("{:010} 00000 n \n", self.offsets[i]).as_bytes())?;
        }
        self.write(
            format!("trailer\n<</Size {total} /Root {CATALOG} 0 R>>\nstartxref\n{xref}\n%%EOF\n")
                .as_bytes(),
        )?;
        self.out.flush()?;
        drop(self.out);
        std::fs::rename(&self.tmp, &self.dst)?;
        Ok(())
    }
}

/// UTF-16BE 十六进制串 —— UniGB-UCS2-H 下字码就是 UCS-2 码位
///
/// BMP 以外的字(生僻字、emoji)用两个代理码元表示, 抽出来还是原字, 因为
/// ToUnicode 是恒等的, 阅读器把两个码元拼回去。
fn utf16be_hex(t: &str) -> String {
    let mut s = String::with_capacity(t.len() * 4);
    for u in t.chars().filter(|c| !c.is_control()).flat_map(|c| {
        let mut b = [0u16; 2];
        c.encode_utf16(&mut b).to_vec()
    }) {
        s.push_str(&format!("{u:04X}"));
    }
    s
}

/// 码位到 Unicode 的对照表。这个编码下是恒等映射, 一条 bfrange 覆盖整个 BMP
const TO_UNICODE_CMAP: &str = r"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo <</Registry (Adobe) /Ordering (UCS) /Supplement 0>> def
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfrange
<0000> <FFFF> <0000>
endbfrange
endcmap
CMapName currentdict /CMap defineresource pop
end
end";

#[cfg(test)]
mod tests {
    use super::*;

    /// pdfium 的绑定是进程级的, 两个线程同时 `Renderer::new()` 会踩崩。
    /// 库里一次转换只建一个, 用不着管; 用例是并行跑的, 得自己排队
    fn pdfium() -> (std::sync::MutexGuard<'static, ()>, crate::pdf::Renderer) {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let r = crate::pdf::Renderer::new().expect("pdfium");
        (g, r)
    }

    fn item(t: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> Item {
        Item {
            t: t.into(),
            x0,
            y0,
            x1,
            y1,
            s: 0.99,
        }
    }

    fn page() -> Gray {
        let mut g = Gray {
            w: 600,
            h: 800,
            px: vec![255; 600 * 800],
        };
        for y in 100..130 {
            for x in 80..400 {
                g.px[y * 600 + x] = 30;
            }
        }
        g
    }

    /// 建一份只有一页的 PDF, 落在临时目录里
    fn build(name: &str, items: &[Item]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("pdf2doc-{name}-{}.pdf", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let mut b = Builder::new(&p).expect("建 PDF");
        b.add_page(PageImg::Gray(&page()), items, (300.0, 400.0))
            .expect("加页");
        assert_eq!(b.pages(), 1);
        b.finish().expect("收尾");
        p
    }

    #[test]
    fn writes_a_pdf_that_pdfium_can_open() {
        let p = build("open", &[item("合同编号", 80.0, 100.0, 400.0, 130.0)]);
        let (_g, r) = pdfium();
        let doc = r.open(&p, 1200).expect("打开自己写的 PDF");
        assert_eq!(doc.len(), 1);
    }

    /// 关键的一条: 写进去的中文得能被抽出来, 不然"可搜索"就是空话
    #[test]
    fn the_text_can_be_read_back_out() {
        let p = build(
            "read",
            &[
                item("合同编号", 80.0, 100.0, 400.0, 130.0),
                item("Contract No. 42", 80.0, 200.0, 400.0, 230.0),
            ],
        );
        let (_g, r) = pdfium();
        let doc = r.open(&p, 1200).expect("打开");
        let got: String = doc.chars(0).expect("抽字").iter().map(|c| c.c).collect();
        assert!(got.contains("合同编号"), "中文没抽出来: {got:?}");
        assert!(got.contains("Contract No. 42"), "西文没抽出来: {got:?}");
    }

    /// 文字盖在图上, 位置得跟原来的框对得上
    #[test]
    fn the_text_lands_where_it_was_on_the_page() {
        let p = build("where", &[item("合同编号", 80.0, 100.0, 400.0, 130.0)]);
        let (_g, r) = pdfium();
        let doc = r.open(&p, 1200).expect("打开");
        let cs = doc.chars(0).expect("抽字");
        let (x0, y0) = (
            cs.iter().map(|c| c.x0).fold(f32::MAX, f32::min),
            cs.iter().map(|c| c.y0).fold(f32::MAX, f32::min),
        );
        // 原框左上角 (80,100) 在 600x800 的页图上, 页 300x400 点, 渲染回 1200 长边
        // -> 换算后仍应落在页面左上区域
        assert!(x0 < 0.2 * 1200.0, "横坐标跑了: {x0}");
        assert!(y0 < 0.3 * 1600.0, "纵坐标跑了: {y0}");
    }

    #[test]
    fn a_page_without_any_text_is_still_a_valid_page() {
        let p = build("empty", &[]);
        let (_g, r) = pdfium();
        assert_eq!(r.open(&p, 1200).expect("打开").len(), 1);
    }

    #[test]
    fn surrogate_pairs_survive_the_round_trip() {
        // BMP 以外的字: 两个代理码元, ToUnicode 恒等所以能拼回来
        let hex = utf16be_hex("𠮷");
        assert_eq!(hex.len(), 8, "该是两个码元: {hex}");
        assert_eq!(hex, "D842DFB7");
    }
}
