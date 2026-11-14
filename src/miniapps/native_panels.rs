//! The native (non-sandboxed) panels a workspace tab can host in place of a
//! mini app: the web reader, the article editor and a user's profile.

use makepad_widgets::*;
use super::surface::SurfaceKey;

/// The template a native panel of `key` is built (and restyled) from.
pub(super) fn native_template(vm: &mut ScriptVm, key: &SurfaceKey) -> ScriptValue {
    match key {
        SurfaceKey::Reader => script_eval!(vm, {mod.widgets.WebBrowser {padding: 0}}),
        SurfaceKey::Profile => script_eval!(vm, {mod.widgets.ProfileSidePanel {}}),
        _ => script_eval!(vm, {mod.widgets.ArticlePanel {padding: 0}}),
    }
}

/// The tab title of a native panel of `key`.
pub(super) fn native_title(key: &SurfaceKey) -> &'static str {
    crate::i18n::tr(match key {
        SurfaceKey::Reader => "Web pages",
        SurfaceKey::Profile => "Profile",
        _ => "Article editor",
    })
}
