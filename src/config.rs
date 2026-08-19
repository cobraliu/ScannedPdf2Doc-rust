//! 转换参数 —— 与 Python 版 convert.py 的 DEFAULTS 一一对应

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // ---- 渲染 / 识别 ----
    /// 渲染长边像素: 越大越准也越慢。实际 dpi 按页面尺寸倒推并夹在 150~300,
    /// 这样 A3 图纸和 A5 单据出来的字号一样大, 阈值才通用
    pub long_edge: u32,

    /// 原生 PDF 直接取自带的文字层, 不走识别
    ///
    /// 关掉它的唯一理由: 手上这份是扫描件, 却被别的工具塞过一层不准的 OCR
    /// 文字层 —— 那层分不出好坏, 宁可自己重认一遍
    pub text_layer: bool,

    /// 识别之前先把扫歪的页面转正
    ///
    /// 只对识别那条路有意义。走文字层的页不会转 —— 那些字的坐标来自 PDF
    /// 文件本身, 图一转就跟坐标对不上了, 何况原生 PDF 本来也不歪
    pub deskew: bool,
    /// 光照不匀的页面先摊平(手机拍的纸一边亮一边暗)
    pub flatten: bool,
    /// 印章/签名/插图照原样裁出来贴进输出
    pub keep_figures: bool,

    // ---- 噪声过滤 ----
    /// 剔除印章/签名一类的短串低置信噪声
    pub drop_stamp: bool,
    /// 短串判噪声的置信度上限
    pub stamp_conf: f32,
    pub drop_header: bool,
    pub drop_footer: bool,
    /// 页眉判定: 相对 y 小于此值
    pub header_y: f32,
    /// 页脚判定: 相对 y 大于此值, 且内容看着像页脚
    ///
    /// 位置只是必要条件。正文本来就能排到页面底部, 光按位置丢会把正文吃掉 ——
    /// 实测 3#线 有 135 行正文落在 0.915 以下, 最长的一行 109 个字
    pub footer_y: f32,

    // ---- 行 / 块 ----
    /// 两个 item 的 y 区间重叠超过此比例算同一视觉行
    pub line_tol: f32,
    /// 行内横向空白超过此值(相对页宽)才把这行当表格行
    pub gutter: f32,
    /// 排到这个相对位置才算"排满", 决定续行能否合并
    pub full_line: f32,
    /// 缩进变化超过此值视为换了层级
    pub x_tol: f32,
    /// 缩进超出块基准这么多就当列表项
    pub bullet_ind: f32,
    /// 列表项缩进的上限: 缩得比这还多就不是列表, 是另起一栏或者靠右摆的
    ///
    /// mark_bullets 是靠"比块基准缩得多"把 OCR 丢掉的 • 找回来的, 只看缩进,
    /// 于是右栏正文和靠右摆的短行全被当成了列表项。实测 3#线 + 配套 一共判出
    /// 159 条, 42 条真列表全落在 0.030~0.059, 剩下 117 条从 0.185 起跳 ——
    /// 右上角的页眉「详细技术描述页码 11 /32」、「合同编号：TKDL20210426」、
    /// 双语对照的右栏。0.10 落在这道空档中间
    pub bullet_ind_max: f32,
    /// 一级缩进占页宽的比例 —— 段落左边比正文基准多出几个, 就缩几级
    ///
    /// 默认 0.035 是 Word 一级 0.74cm 折成 A4 页宽(21cm)的结果, 两头对齐,
    /// 量出来多少级写回去就是多少级
    pub ind_step: f32,
    /// 最多缩几级, 再深下去这一行就没地方写字了
    ///
    /// 15 级是 11.1cm, A4 正文宽 16cm, 还剩 4.9cm 够写一行。原先写 5 是照着列表那 6 级
    /// 定的, 可正文没有这个约束 —— 3#线 左右两栏的页面右栏起点在页宽 0.53,
    /// 一律拍到第 5 级就跟左栏挤在一起: 96 页里 269 段堆在第 5 级上, 第 4 级
    /// 只有 22 段。缩到顶还放不下的短行按靠右摆处理(见 render::write_text)
    pub ind_max: u8,
    /// 无框线表格至少要这么多行才成表
    pub min_tbl_rows: usize,

    // ---- 开关 ----
    /// 多列版面还原成无边框表格
    pub tables: bool,
    /// 画了框线的表格照框线还原(含合并单元格)
    pub grid_tables: bool,
    /// 页与页之间插一条「原第 N 页」, 对照原件时好定位
    pub page_marker: bool,

    // ---- 字体 ----
    pub zh_font: String,
    pub en_font: String,
    pub font_size: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            long_edge: 2560,
            text_layer: true,
            deskew: true,
            flatten: true,
            keep_figures: true,

            drop_stamp: true,
            stamp_conf: 0.88,
            drop_header: true,
            drop_footer: true,
            header_y: 0.125,
            footer_y: 0.85,

            line_tol: 0.45,
            gutter: 0.035,
            full_line: 0.78,
            x_tol: 0.025,
            bullet_ind: 0.030,
            bullet_ind_max: 0.10,
            ind_step: 0.035,
            ind_max: 15,
            min_tbl_rows: 2,

            tables: true,
            grid_tables: true,
            page_marker: true,

            zh_font: "宋体".into(),
            en_font: "Times New Roman".into(),
            font_size: 10.5,
        }
    }
}

/// 要出哪几份
///
/// 原来是三选一的枚举(Word / Excel / 两份都要)。加了 Markdown 和可搜索 PDF
/// 之后"两份都要"这说法就不够用了 —— 一次识别的结果落成几种文件本来就是
/// 互不相干的事, 改成一组开关。
///
/// 旧配置里存的是 `"Docx"` / `"Xlsx"` / `"Both"` 这样的字符串, 反序列化时
/// 照旧认: 升一次级把用户存了半年的设置打回默认, 是不该发生的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Format {
    pub docx: bool,
    pub xlsx: bool,
    /// 纯文本的结构版: 标题分级、清单、表格都在, 版式细节不留
    pub md: bool,
    /// 可搜索 PDF: 版面一个像素不动, 底下垫一层看不见的文字
    pub pdf: bool,
}

impl Format {
    pub const NONE: Self = Self {
        docx: false,
        xlsx: false,
        md: false,
        pdf: false,
    };
    pub const DOCX: Self = Self {
        docx: true,
        ..Self::NONE
    };

    /// 认逗号(或加号、空格)分隔的一串: `docx,pdf`
    ///
    /// 有一个词不认识就整个不认 —— 命令行上打错一个字母, 默默少出一份文件
    /// 比直接报错难发现得多。
    pub fn parse(s: &str) -> Option<Self> {
        let mut f = Self::NONE;
        let mut n = 0;
        for w in s.split([',', '+', ' ', '/']).filter(|w| !w.is_empty()) {
            n += 1;
            match w.to_ascii_lowercase().as_str() {
                "docx" | "word" => f.docx = true,
                "xlsx" | "excel" => f.xlsx = true,
                "md" | "markdown" => f.md = true,
                "pdf" => f.pdf = true,
                // 老写法: 那会儿只有两种输出
                "both" => {
                    f.docx = true;
                    f.xlsx = true;
                }
                "all" => {
                    f = Self {
                        docx: true,
                        xlsx: true,
                        md: true,
                        pdf: true,
                    }
                }
                _ => return None,
            }
        }
        (n > 0 && f != Self::NONE).then_some(f)
    }

    /// 一样都不选等于什么都不用干
    pub fn is_none(self) -> bool {
        self == Self::NONE
    }
}

impl Default for Format {
    fn default() -> Self {
        Self::DOCX
    }
}

impl<'de> Deserialize<'de> for Format {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            /// v0.1.6 及以前: 一个枚举名
            Old(String),
            New {
                #[serde(default)]
                docx: bool,
                #[serde(default)]
                xlsx: bool,
                #[serde(default)]
                md: bool,
                #[serde(default)]
                pdf: bool,
            },
        }
        Ok(match Repr::deserialize(d)? {
            Repr::Old(s) => Format::parse(&s).unwrap_or_default(),
            Repr::New {
                docx,
                xlsx,
                md,
                pdf,
            } => Format {
                docx,
                xlsx,
                md,
                pdf,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_command_line_spellings() {
        assert_eq!(Format::parse("docx"), Some(Format::DOCX));
        assert_eq!(Format::parse("word"), Some(Format::DOCX));
        assert_eq!(
            Format::parse("docx,pdf"),
            Some(Format {
                docx: true,
                pdf: true,
                ..Format::NONE
            })
        );
        assert_eq!(Format::parse("md+xlsx"), Format::parse("xlsx, markdown"));
        assert_eq!(
            Format::parse("all"),
            Some(Format {
                docx: true,
                xlsx: true,
                md: true,
                pdf: true
            })
        );
        // 打错字要报出来, 不能悄悄少出一份
        assert_eq!(Format::parse("docs"), None);
        assert_eq!(Format::parse(""), None);
    }

    /// 老设置文件里存的是枚举名, 升级不该把用户的设置清空
    #[test]
    fn still_reads_settings_written_by_the_old_version() {
        let old: Format = serde_json::from_str("\"Both\"").expect("旧写法");
        assert!(old.docx && old.xlsx && !old.md && !old.pdf);
        let old: Format = serde_json::from_str("\"Xlsx\"").expect("旧写法");
        assert_eq!(
            old,
            Format {
                xlsx: true,
                ..Format::NONE
            }
        );
        // 认不出来的字符串退回默认, 不是报错 —— 手改坏了一行不该起不来
        let bad: Format = serde_json::from_str("\"Whatever\"").expect("认不出的旧写法");
        assert_eq!(bad, Format::DOCX);
    }

    #[test]
    fn round_trips_through_json() {
        let f = Format {
            docx: true,
            md: true,
            ..Format::NONE
        };
        let s = serde_json::to_string(&f).expect("写");
        assert_eq!(serde_json::from_str::<Format>(&s).expect("读"), f);
    }
}
