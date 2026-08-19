//! .docx 输出 —— 直接拼 OOXML 再打包成 zip
//!
//! Rust 没有 python-docx 那样成熟的库, 但 docx 本身就是几个 XML 打个 zip。
//! 我们要用的东西不多: 段落、表格(含 gridSpan/vMerge)、OMML 分式、跨页重复
//! 表头。手写反而比套一层薄封装更好控制。

use anyhow::Result;
use std::io::Write;
use std::path::Path;

use crate::config::Config;
use crate::layout::grid;
use crate::layout::line::FRAC_SEP;

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// 1 cm = 567 twips
pub const CM: f32 = 567.0;

pub fn esc(t: &str) -> String {
    t.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 单元格上除了文字和宽度之外的那几件事
///
/// 攒成一个结构而不是一路加参数: `cell()` 本来就有五个位置参数了, 再加两个,
/// 调用处全是没名字的 true/false/None, 看不出哪个是哪个。
#[derive(Clone, Copy, Default)]
pub struct CellOpt {
    /// 横跨几列; 0 和 1 都表示不跨
    pub span: usize,
    /// Some(true)=纵向合并的起始格, Some(false)=被并掉的格
    pub vmerge: Option<bool>,
    /// 文字上下左右都居中
    pub center: bool,
    /// 四条边画不画(顺序 上 右 下 左); None = 跟着表格设的走
    pub edges: Option<[bool; 4]>,
}

/// 段落的横向对齐
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    /// 靠右摆的短行 —— 页眉右上角的页码、合同编号、落款
    Right,
}

impl Align {
    fn ppr(self) -> &'static str {
        match self {
            Align::Left => "",
            Align::Center => r#"<w:jc w:val="center"/>"#,
            Align::Right => r#"<w:jc w:val="right"/>"#,
        }
    }
}

/// 一段文字的排版属性
#[derive(Clone)]
pub struct Fmt {
    pub size: f32,
    pub bold: bool,
    pub color: Option<&'static str>,
}

impl Fmt {
    pub fn new(size: f32) -> Self {
        Self {
            size,
            bold: false,
            color: None,
        }
    }
    pub fn bold(mut self, b: bool) -> Self {
        self.bold = b;
        self
    }
    pub fn color(mut self, c: &'static str) -> Self {
        self.color = Some(c);
        self
    }
}

pub struct Docx {
    body: String,
    cfg: Config,
    landscape: bool,
    /// 表格在 body 里的插入点 —— 跨页续表要往已写出的表里追加行
    tables: Vec<String>,
    /// 嵌进去的图片, 打包时写成 word/media/imageN.png
    media: Vec<Vec<u8>>,
}

/// body 里用这个占位符标记"第 n 张表格的位置", 收尾时替换成表格 XML
fn slot(i: usize) -> String {
    format!("\u{1}TBL{i}\u{1}")
}

impl Docx {
    pub fn new(cfg: &Config, landscape: bool) -> Self {
        Self {
            body: String::new(),
            cfg: cfg.clone(),
            landscape,
            tables: Vec::new(),
            media: Vec::new(),
        }
    }

    /// 当前版面的可用正文宽度(twips); 横版页比竖版宽一半, 表格列宽得跟着变
    pub fn usable_w(&self) -> f32 {
        let page = if self.landscape { 29.7 } else { 21.0 };
        (page - 5.0) * CM
    }

    /// 整张纸的宽度(twips), 含页边距 —— 贴图时按它换算比例
    pub fn page_w(&self) -> f32 {
        self.usable_w() + 5.0 * CM
    }

    fn run_props(&self, f: &Fmt) -> String {
        let sz = (f.size * 2.0).round() as i32;
        let mut s = format!(
            r#"<w:rFonts w:ascii="{en}" w:hAnsi="{en}" w:eastAsia="{zh}"/>"#,
            en = esc(&self.cfg.en_font),
            zh = esc(&self.cfg.zh_font)
        );
        if f.bold {
            s.push_str("<w:b/><w:bCs/>");
        }
        if let Some(c) = f.color {
            s.push_str(&format!(r#"<w:color w:val="{c}"/>"#));
        }
        s.push_str(&format!(r#"<w:sz w:val="{sz}"/><w:szCs w:val="{sz}"/>"#));
        format!("<w:rPr>{s}</w:rPr>")
    }

    /// 把文本写成若干 run; 碰到分式占位符就插一段 OMML 公式
    ///
    /// split 之后下标 0,3,6... 是普通文本, 紧跟的两段是分子分母。
    fn runs(&self, text: &str, f: &Fmt) -> String {
        let parts: Vec<&str> = text.split(FRAC_SEP).collect();
        let mut out = String::new();
        let mut i = 0;
        while i < parts.len() {
            if !parts[i].is_empty() {
                out.push_str(&format!(
                    r#"<w:r>{}<w:t xml:space="preserve">{}</w:t></w:r>"#,
                    self.run_props(f),
                    esc(parts[i])
                ));
            }
            if i + 2 < parts.len() {
                out.push_str(&self.omml_frac(parts[i + 1], parts[i + 2], f.size));
            }
            i += 3;
        }
        out
    }

    /// OMML 分式: Word 里显示成真正的上下叠排, 双击能进公式编辑器
    ///
    /// 字体必须是 Cambria Math —— 换别的字体 Word 不认它是公式字形。
    fn omml_frac(&self, num: &str, den: &str, size: f32) -> String {
        let sz = (size * 2.0).round() as i32;
        let r = |t: &str| {
            format!(
                concat!(
                    r#"<m:r><w:rPr><w:rFonts w:ascii="Cambria Math" w:hAnsi="Cambria Math"/>"#,
                    r#"<w:sz w:val="{sz}"/><w:szCs w:val="{sz}"/></w:rPr>"#,
                    r#"<m:t xml:space="preserve">{t}</m:t></m:r>"#
                ),
                sz = sz,
                t = esc(t)
            )
        };
        format!(
            "<m:oMath><m:f><m:num>{}</m:num><m:den>{}</m:den></m:f></m:oMath>",
            r(num),
            r(den)
        )
    }

    /// 段落
    pub fn para(&mut self, text: &str, f: &Fmt, indent_lv: u8, align: Align, bullet: bool) {
        let mut ppr = String::from(
            r#"<w:spacing w:before="0" w:after="60" w:line="276" w:lineRule="auto"/>"#,
        );
        let step = (0.74 * CM).round() as i32;
        if bullet {
            // 缩进得直接写在段落上, 不能光靠 w:ilvl
            //
            // OOXML 里三者的优先级是 编号定义 < 段落样式 < 段落直接格式。
            // ListBullet 样式带着 w:ind left=420, 它压过编号里那一级自己的
            // w:ind —— 只挂 ilvl 的话, 第 3 级的 • 照样贴在第 1 级的位置上。
            //
            // ilvl 只挑用哪个符号, 位置由 w:ind 说了算, 所以两者不必同一个数:
            // numbering.xml 只声明了 BULLET_LVLS 级, ilvl 引用没声明的级 Word
            // 会把整段的编号丢掉, 但缩进照量到的写就行
            let lv = indent_lv.min(BULLET_LVLS - 1);
            let ind = step * (indent_lv as i32 + 1);
            ppr.push_str(&format!(
                r#"<w:pStyle w:val="ListBullet"/><w:numPr><w:ilvl w:val="{lv}"/><w:numId w:val="1"/></w:numPr><w:ind w:left="{ind}" w:hanging="{step}"/>"#
            ));
        } else if indent_lv > 0 {
            let ind = step * indent_lv as i32;
            ppr.push_str(&format!(r#"<w:ind w:left="{ind}"/>"#));
        }
        ppr.push_str(align.ppr());
        let runs = self.runs(text, f);
        self.body
            .push_str(&format!("<w:p><w:pPr>{ppr}</w:pPr>{runs}</w:p>"));
    }

    /// 插一张图, 宽高按 twips 给
    ///
    /// 走内联(`wp:inline`)而不是浮动: 浮动图得指定锚点和环绕方式, 在一份重建
    /// 出来的文档里没有可靠的锚可挂, Word 和 WPS 对环绕的解释也不一致。内联
    /// 图老老实实占一个段落的位置, 到哪个读者手里都是同一个样子。
    ///
    /// 命名空间 wp / a / pic 就地声明在用到的元素上 —— 这是 OOXML 里的常规
    /// 写法, 也免得为了几张图去动 w:document 的根元素。
    pub fn image(&mut self, png: Vec<u8>, w_tw: i32, h_tw: i32, align: Align) {
        // rId1 是 styles, rId2 是 numbering, 图片从 rId3 起
        let rid = self.media.len() + 3;
        let n = self.media.len() + 1;
        self.media.push(png);
        // 1 inch = 1440 twips = 914400 EMU
        let (cx, cy) = (w_tw as i64 * 635, h_tw as i64 * 635);
        self.body.push_str(&format!(
            concat!(
                r#"<w:p><w:pPr><w:spacing w:before="60" w:after="60"/>{}</w:pPr><w:r><w:drawing>"#,
                r#"<wp:inline distT="0" distB="0" distL="0" distR="0" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing">"#,
                r#"<wp:extent cx="{}" cy="{}"/><wp:docPr id="{}" name="Figure {}"/>"#,
                r#"<a:graphic xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">"#,
                r#"<a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
                r#"<pic:pic xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">"#,
                r#"<pic:nvPicPr><pic:cNvPr id="{}" name="Figure {}"/><pic:cNvPicPr/></pic:nvPicPr>"#,
                r#"<pic:blipFill><a:blip r:embed="rId{}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill>"#,
                r#"<pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{}" cy="{}"/></a:xfrm>"#,
                r#"<a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr>"#,
                r#"</pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#,
            ),
            align.ppr(),
            cx,
            cy,
            n,
            n,
            n,
            n,
            rid,
            cx,
            cy
        ));
    }

    pub fn blank(&mut self) {
        self.body.push_str("<w:p/>");
    }

    /// 新起一张表, 返回表格句柄(下标)
    ///
    /// 框线表除了挂 TableGrid 这个样式, 还把六条边显式写一遍。样式引用是
    /// "指过去"而不是"画出来", WPS、Pages、Google Docs 这些不解析表格样式的
    /// 读者拿到的就是一张没有线的表 —— 显式写一遍谁都赖不掉。
    pub fn new_table(&mut self, widths: &[i32], grid_style: bool) -> usize {
        let id = self.tables.len();
        let mut xml = String::from("<w:tbl><w:tblPr>");
        if grid_style {
            xml.push_str(r#"<w:tblStyle w:val="TableGrid"/>"#);
        }
        // tblPr 里子元素的先后顺序是 schema 定死的: tblW 之后是 tblBorders,
        // 再往后才是 tblLayout。写反了严格一点的读者会报文档损坏
        xml.push_str(r#"<w:tblW w:w="0" w:type="auto"/>"#);
        xml.push_str("<w:tblBorders>");
        for e in ["top", "left", "bottom", "right", "insideH", "insideV"] {
            xml.push_str(&if grid_style {
                format!(r#"<w:{e} w:val="single" w:sz="4" w:space="0" w:color="auto"/>"#)
            } else {
                format!(r#"<w:{e} w:val="none" w:sz="0" w:space="0" w:color="auto"/>"#)
            });
        }
        xml.push_str("</w:tblBorders>");
        xml.push_str(r#"<w:tblLayout w:type="fixed"/>"#);
        xml.push_str("</w:tblPr><w:tblGrid>");
        for w in widths {
            xml.push_str(&format!(r#"<w:gridCol w:w="{w}"/>"#));
        }
        xml.push_str("</w:tblGrid>");
        self.tables.push(xml);
        self.body.push_str(&slot(id));
        id
    }

    /// 往表里追加一行原始 XML
    pub fn push_row(&mut self, table: usize, row_xml: &str) {
        self.tables[table].push_str(row_xml);
    }

    /// 一个单元格
    ///
    /// `span` 横跨几列, `vmerge` 取 Some(true)=起始格 / Some(false)=被并掉的格。
    /// 文本里的 '\n' 写成多个段落 —— 单元格里换行只能这么写。
    pub fn cell(&self, text: &str, w: i32, f: &Fmt, o: &CellOpt) -> String {
        let mut tcpr = format!(r#"<w:tcW w:w="{w}" w:type="dxa"/>"#);
        if o.span > 1 {
            tcpr.push_str(&format!(r#"<w:gridSpan w:val="{}"/>"#, o.span));
        }
        match o.vmerge {
            Some(true) => tcpr.push_str(r#"<w:vMerge w:val="restart"/>"#),
            Some(false) => tcpr.push_str("<w:vMerge/>"),
            None => {}
        }
        // tcPr 的顺序同样是定死的: vMerge 之后 tcBorders, 再往后才是 vAlign
        if let Some(e) = o.edges {
            tcpr.push_str("<w:tcBorders>");
            for (name, on) in [
                ("top", e[grid::TOP]),
                ("left", e[grid::LEFT]),
                ("bottom", e[grid::BOTTOM]),
                ("right", e[grid::RIGHT]),
            ] {
                tcpr.push_str(&if on {
                    format!(r#"<w:{name} w:val="single" w:sz="4" w:space="0" w:color="auto"/>"#)
                } else {
                    format!(r#"<w:{name} w:val="none" w:sz="0" w:space="0" w:color="auto"/>"#)
                });
            }
            tcpr.push_str("</w:tcBorders>");
        }
        let center = o.center;
        if center {
            tcpr.push_str(r#"<w:vAlign w:val="center"/>"#);
        }
        let mut inner = String::new();
        for part in text.split('\n') {
            let jc = if center {
                r#"<w:jc w:val="center"/>"#
            } else {
                ""
            };
            inner.push_str(&format!(
                r#"<w:p><w:pPr><w:spacing w:before="0" w:after="0" w:line="252" w:lineRule="auto"/>{jc}</w:pPr>{}</w:p>"#,
                if part.is_empty() { String::new() } else { self.runs(part, f) }
            ));
        }
        format!("<w:tc><w:tcPr>{tcpr}</w:tcPr>{inner}</w:tc>")
    }

    /// 一行; `header` 为真时标 tblHeader, 表格跨页 Word 会自动重复这一行
    pub fn row(&self, cells: &str, header: bool) -> String {
        let trpr = if header {
            r#"<w:trPr><w:tblHeader w:val="true"/></w:trPr>"#
        } else {
            ""
        };
        format!("<w:tr>{trpr}{cells}</w:tr>")
    }

    /// 跨页续表时把「原第 N 页」做成表内整行, 免得为了插标记把表切断
    pub fn marker_row(&self, page: &str, ncols: usize, total_w: i32) -> String {
        let f = Fmt::new(8.0).color("999999");
        let o = CellOpt {
            span: ncols,
            center: true,
            ..Default::default()
        };
        let cell = self.cell(&crate::tr!(crate::i18n::K::MarkPage, page), total_w, &f, &o);
        self.row(&cell, false)
    }

    pub fn save(mut self, path: &Path) -> Result<()> {
        for (i, t) in self.tables.iter().enumerate() {
            self.body = self.body.replace(&slot(i), &format!("{t}</w:tbl>"));
        }
        let (pw, ph, orient) = if self.landscape {
            (16838, 11906, r#" w:orient="landscape""#)
        } else {
            (11906, 16838, "")
        };
        let doc = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="{W_NS}" xmlns:m="{M_NS}" xmlns:r="{R_NS}"><w:body>{}
<w:sectPr><w:pgSz w:w="{pw}" w:h="{ph}"{orient}/><w:pgMar w:top="1418" w:right="1418" w:bottom="1418" w:left="1418" w:header="851" w:footer="992" w:gutter="0"/></w:sectPr>
</w:body></w:document>"#,
            self.body
        );

        // create_new 而不是 create: 调用方已经挑过没被占用的名字了, 这里是第二道
        // 闸 —— 万一挑名和落盘之间被人塞了个文件进来, 宁可报错也不覆盖
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let mut zip = zip::ZipWriter::new(file);
        let opt: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

        zip.start_file("[Content_Types].xml", opt)?;
        zip.write_all(content_types(!self.media.is_empty()).as_bytes())?;
        zip.start_file("_rels/.rels", opt)?;
        zip.write_all(RELS.as_bytes())?;
        zip.start_file("word/_rels/document.xml.rels", opt)?;
        zip.write_all(doc_rels(self.media.len()).as_bytes())?;
        zip.start_file("word/styles.xml", opt)?;
        zip.write_all(styles(&self.cfg).as_bytes())?;
        zip.start_file("word/numbering.xml", opt)?;
        zip.write_all(numbering().as_bytes())?;
        zip.start_file("word/document.xml", opt)?;
        zip.write_all(doc.as_bytes())?;
        for (i, png) in self.media.iter().enumerate() {
            // 图片本身已经是 PNG(自带 deflate), 再压一遍纯属白费, 直接存
            let raw: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file(format!("word/media/image{}.png", i + 1), raw)?;
            zip.write_all(png)?;
        }
        zip.finish()?;
        Ok(())
    }
}

fn content_types(png: bool) -> String {
    let png = if png {
        r#"<Default Extension="png" ContentType="image/png"/>"#
    } else {
        ""
    };
    CONTENT_TYPES.replace("<!--PNG-->", png)
}

/// styles 和 numbering 是固定的两条, 图片一张一条接在后面
fn doc_rels(images: usize) -> String {
    const IMG: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
    let mut s = String::from(DOC_RELS_HEAD);
    for i in 0..images {
        s.push_str(&format!(
            r#"<Relationship Id="rId{}" Type="{IMG}" Target="media/image{}.png"/>"#,
            i + 3,
            i + 1
        ));
    }
    s.push_str("</Relationships>");
    s
}

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<!--PNG-->
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
<Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/>
</Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#;

const DOC_RELS_HEAD: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/>"#;

/// Table Grid 样式必须显式定义, 否则 Word 打开时框线不显示
fn styles(cfg: &Config) -> String {
    let sz = (cfg.font_size * 2.0).round() as i32;
    let mut borders = String::new();
    for e in ["top", "left", "bottom", "right", "insideH", "insideV"] {
        borders.push_str(&format!(
            r#"<w:{e} w:val="single" w:sz="4" w:space="0" w:color="auto"/>"#
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{W_NS}">
<w:docDefaults><w:rPrDefault><w:rPr>
<w:rFonts w:ascii="{en}" w:hAnsi="{en}" w:eastAsia="{zh}"/><w:sz w:val="{sz}"/><w:szCs w:val="{sz}"/>
</w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="ListBullet"><w:name w:val="List Bullet"/><w:basedOn w:val="Normal"/>
<w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr><w:ind w:left="420" w:hanging="420"/></w:pPr></w:style>
<w:style w:type="table" w:styleId="TableGrid"><w:name w:val="Table Grid"/>
<w:tblPr><w:tblBorders>{borders}</w:tblBorders></w:tblPr></w:style>
</w:styles>"#,
        en = esc(&cfg.en_font),
        zh = esc(&cfg.zh_font)
    )
}

/// 编号定义里放了几级项目符号
///
/// 嵌套清单最深见过五级(Conclusion-for-QA 是 1. -> a. -> i. -> 1. -> a.),
/// 六级留一点富余。Word 自己的默认列表是九级, 少定义几级只是深处不再变符号,
/// 不会出错。
const BULLET_LVLS: u8 = 6;

/// 每级的符号与字体: • / o / ▪ 三个一轮, 跟 Word 默认列表一致
const BULLET_MARKS: [(&str, &str); 3] = [("•", "Symbol"), ("o", "Courier New"), ("▪", "Wingdings")];

/// 六级项目符号 —— 每级各自的符号和缩进
///
/// 每一级都得在这儿定义出来: 段落里写了 w:ilvl="3" 而编号定义只到 0, Word 会
/// 把这一段的编号整个丢掉, 符号直接不显示。
fn numbering() -> String {
    let step = (0.74 * CM).round() as i32;
    let lvls: String = (0..BULLET_LVLS)
        .map(|i| {
            let (mark, font) = BULLET_MARKS[i as usize % BULLET_MARKS.len()];
            let ind = step * (i as i32 + 1);
            format!(
                r#"<w:lvl w:ilvl="{i}"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="{mark}"/>
<w:lvlJc w:val="left"/><w:pPr><w:ind w:left="{ind}" w:hanging="{step}"/></w:pPr>
<w:rPr><w:rFonts w:ascii="{font}" w:hAnsi="{font}" w:hint="default"/></w:rPr></w:lvl>
"#
            )
        })
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="hybridMultilevel"/>
{lvls}</w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
</w:numbering>"#
    )
}
