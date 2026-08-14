//! 回归测试: 一个进程里能建第二个 Renderer
//!
//! pdfium-render 的绑定是进程级全局的, 第二次 bind_to_library 一律返回
//! AlreadyInitialized。GUI 里"转完一批再转一批"会新建 Converter, 不认这个错
//! 第二次就起不来 —— 曾经就这么炸过, 报"绑定 pdfium 失败"。
//!
//! 需要 libpdfium 在场(vendor/ 或 PDFIUM_LIB), 找不到就跳过: CI 之外的开发机
//! 未必备着这个二进制, 不该因此判红。

use scannedpdf2doc::pdf::Renderer;

#[test]
fn renderer_can_be_created_twice() {
    let Ok(first) = Renderer::new() else {
        eprintln!("跳过: 没找到 libpdfium");
        return;
    };
    let second = Renderer::new().expect("第二个 Renderer 应当复用已有绑定, 而不是报错");
    drop((first, second));
}
