//! 转换参数 —— 与 Python 版 convert.py 的 DEFAULTS 一一对应

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    // ---- 渲染 / 识别 ----
    /// 渲染长边像素: 越大越准也越慢。实际 dpi 按页面尺寸倒推并夹在 150~300,
    /// 这样 A3 图纸和 A5 单据出来的字号一样大, 阈值才通用
    pub long_edge: u32,

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
    /// 一级缩进占页宽的比例 —— 段落左边比正文基准多出几个, 就缩几级
    ///
    /// 默认 0.035 是 Word 一级 0.74cm 折成 A4 页宽(21cm)的结果, 两头对齐,
    /// 量出来多少级写回去就是多少级
    pub ind_step: f32,
    /// 最多缩几级, 再深下去正文就没地方了
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
            ind_step: 0.035,
            ind_max: 5,
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

/// 输出格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Docx,
    Xlsx,
    /// 同一次识别出两份
    Both,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "docx" | "word" => Some(Format::Docx),
            "xlsx" | "excel" => Some(Format::Xlsx),
            "both" | "all" => Some(Format::Both),
            _ => None,
        }
    }

    pub fn wants_docx(self) -> bool {
        matches!(self, Format::Docx | Format::Both)
    }

    pub fn wants_xlsx(self) -> bool {
        matches!(self, Format::Xlsx | Format::Both)
    }
}
