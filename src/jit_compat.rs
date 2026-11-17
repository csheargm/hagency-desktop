//! JIT PORT: small fork-only `Cx` helpers rebuilt on upstream makepad.

use makepad_widgets::*;

/// The fork queued a system notification that macOS ignored, and upstream
/// has no notification op; on macOS this now shows one (see
/// `shared::desktop_notification`).
pub fn show_notification(_cx: &mut Cx, title: &str, body: &str) {
    crate::shared::desktop_notification::show(title, body);
}

/// Redraw `pass` and every pass it is drawn into, up to its window.
pub fn redraw_pass_and_parent_passes(cx: &mut Cx, pass: DrawPassId) {
    let mut walk = pass;
    loop {
        if let Some(list) = cx.passes[walk].main_draw_list_id {
            cx.redraw_list_and_children(list);
        }
        match cx.passes[walk].parent.clone() {
            CxDrawPassParent::DrawPass(next) => walk = next,
            _ => break,
        }
    }
}
