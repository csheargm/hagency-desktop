//! Main-thread inspection of real native webview attachment in offline fixtures.
use makepad_widgets::*;
use std::path::PathBuf;

pub fn inspect(cx: &Cx, window_ids: Vec<WindowId>, output: PathBuf) {
    let browsers = cx.system_browser_count();
    dispatch2::DispatchQueue::main().exec_async(move || {
        use makepad_widgets::makepad_platform::os::apple::{
            apple_sys::*, macos::macos_app::try_with_macos_app,
        };
        use makepad_widgets::makepad_platform::makepad_objc_sys::{class, msg_send};
        unsafe fn webviews(view: ObjcId, parent_visible: bool) -> (usize, usize) {
            let hidden: BOOL = unsafe { msg_send![view, isHidden] };
            let visible = parent_visible && hidden != YES;
            let web: BOOL = unsafe { msg_send![view, isKindOfClass: class!(WKWebView)] };
            if web == YES { return (1, usize::from(visible)); }
            let children: ObjcId = unsafe { msg_send![view, subviews] };
            let count: usize = unsafe { msg_send![children, count] };
            (0..count).fold((0, 0), |(attached, shown), index| unsafe {
                let child: ObjcId = msg_send![children, objectAtIndex: index];
                let (child_attached, child_shown) = webviews(child, visible);
                (attached + child_attached, shown + child_shown)
            })
        }
        let windows: Vec<_> = window_ids.into_iter().map(|window| {
            let counts = try_with_macos_app(|app| app.cocoa_window_for_id(window)).flatten()
                .map(|native| unsafe {
                    let view: ObjcId = msg_send![native, contentView];
                    webviews(view, true)
                }).unwrap_or_default();
            serde_json::json!({"window":window.id(),"attached_webviews":counts.0,"visible_webviews":counts.1})
        }).collect();
        std::fs::write(output, serde_json::json!({"browsers":browsers,"windows":windows}).to_string()).unwrap();
    });
}
