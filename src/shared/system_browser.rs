//! JIT PORT: the fork's `cx.system_browser(id)` API, rebuilt on upstream
//! makepad native layers (`NativeContent::Web`).
//!
//! The fork drove one OS web view per `SystemBrowserId` through platform ops
//! and reported navigation as `NativeSystemBrowserNavigation` actions. Upstream
//! makepad has no such ops; its web view is a native layer that is placed by
//! drawing it into a draw list every frame. This module keeps the app's call
//! sites unchanged:
//! - `spawn` / `spawn_navigable` create the layer;
//! - `update(area, visible)` (called from `draw_walk`) draws it over `area`;
//! - `detach` hides it by skipping the draw, keeping page and history;
//! - native layer events become `NativeSystemBrowserNavigation` actions,
//!   emitted by [`handle_event`], which the app calls first in its event loop.
//!
//! Gaps against the fork: no navigation policy (`spawn` cannot pin a card to
//! one document), no JS bridge, no WebView2 data directory.

use std::collections::HashMap;

use makepad_widgets::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SystemBrowserId(pub LiveId);

impl From<LiveId> for SystemBrowserId {
    fn from(value: LiveId) -> Self {
        Self(value)
    }
}

/// A system-browser document navigated. Carries the browser it came from.
#[derive(Clone, Debug, Default)]
pub struct NativeSystemBrowserNavigation {
    pub browser_id: u64,
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub error: Option<String>,
}

struct Browser {
    layer: NativeLayer,
    url: String,
    title: String,
    loading: bool,
    detached: bool,
}

#[derive(Default)]
struct Browsers {
    map: HashMap<LiveId, Browser>,
}

fn browsers(cx: &mut Cx) -> &mut Browsers {
    if !cx.has_global::<Browsers>() {
        cx.set_global(Browsers::default());
    }
    cx.get_global::<Browsers>()
}

pub trait SystemBrowserCx {
    fn system_browser(&mut self, id: impl Into<SystemBrowserId>) -> CxSystemBrowser<'_>;
}

impl SystemBrowserCx for Cx {
    fn system_browser(&mut self, id: impl Into<SystemBrowserId>) -> CxSystemBrowser<'_> {
        CxSystemBrowser { cx: self, id: id.into() }
    }
}

pub struct CxSystemBrowser<'a> {
    cx: &'a mut Cx,
    id: SystemBrowserId,
}

impl CxSystemBrowser<'_> {
    pub fn id(&self) -> SystemBrowserId {
        self.id
    }

    /// The fork pinned this browser to one document. Native layers have no
    /// navigation policy yet, so it navigates like `spawn_navigable`.
    pub fn spawn(&mut self, url: &str) {
        self.spawn_navigable(url);
    }

    pub fn spawn_navigable(&mut self, url: &str) {
        self.close();
        let layer = self.cx.native_layer_new(NativeContent::Web { url: url.to_string() });
        browsers(self.cx).map.insert(self.id.0, Browser {
            layer,
            url: url.to_string(),
            title: String::new(),
            loading: true,
            detached: false,
        });
        self.cx.redraw_all();
    }

    /// WebView2 profile selection (Windows); native layers pick their own.
    #[allow(unused)]
    pub fn set_data_directory(&mut self, _path: std::path::PathBuf) {}

    /// Hide the browser until the next visible `update`, keeping its page.
    pub fn detach(&mut self) {
        if let Some(browser) = browsers(self.cx).map.get_mut(&self.id.0) {
            if !browser.detached {
                browser.detached = true;
                self.cx.redraw_all();
            }
        }
    }

    pub fn set_url(&mut self, url: &str, replace: bool) {
        if let Some(layer) = self.layer_id() {
            self.cx.native_layer(layer).navigate(url, replace);
        }
    }

    pub fn history_go(&mut self, delta: i32) {
        if let Some(layer) = self.layer_id() {
            self.cx.native_layer(layer).history(delta);
        }
    }

    pub fn close(&mut self) {
        if let Some(browser) = browsers(self.cx).map.remove(&self.id.0) {
            self.cx.native_layer(browser.layer.id()).close();
            self.cx.redraw_all();
        }
    }

    /// Place the browser over `area`. Only a draw context can place a native
    /// layer, so this is a no-op outside `draw_walk` (see [`update`]).
    pub fn update(&mut self, area: Area, visible: bool) {
        let _ = (area, visible);
    }

    fn layer_id(&mut self) -> Option<NativeLayerId> {
        browsers(self.cx).map.get(&self.id.0).map(|browser| browser.layer.id())
    }
}

/// Draw-time placement: what the fork's `update(area, visible)` did. Call
/// sites in `draw_walk` resolve here through [`SystemBrowserCx2d`].
pub struct Cx2dSystemBrowser<'a, 'b, 'c> {
    cx: &'a mut Cx2d<'b, 'c>,
    id: SystemBrowserId,
}

pub trait SystemBrowserCx2d<'b, 'c> {
    fn system_browser(&mut self, id: impl Into<SystemBrowserId>) -> Cx2dSystemBrowser<'_, 'b, 'c>;
}

impl<'b, 'c> SystemBrowserCx2d<'b, 'c> for Cx2d<'b, 'c> {
    fn system_browser(&mut self, id: impl Into<SystemBrowserId>) -> Cx2dSystemBrowser<'_, 'b, 'c> {
        Cx2dSystemBrowser { cx: self, id: id.into() }
    }
}

impl Cx2dSystemBrowser<'_, '_, '_> {
    pub fn update(&mut self, area: Area, visible: bool) {
        let id = self.id.0;
        let cx: &mut Cx = self.cx;
        let Some(browser) = browsers(cx).map.get_mut(&id) else { return };
        if visible {
            browser.detached = false;
        }
        if !visible || browser.detached || !area.is_valid(self.cx) {
            return;
        }
        let rect = area.rect(self.cx);
        if rect.size.x <= 0.0 || rect.size.y <= 0.0 {
            return;
        }
        // Take the layer out so it can be drawn while `cx` is borrowed.
        let Some(browser) = browsers(self.cx).map.remove(&id) else { return };
        browser.layer.draw(self.cx, rect, NativeLayerStyle::default());
        browsers(self.cx).map.insert(id, browser);
    }
}

/// Turn native layer events into the fork's navigation actions. Call this at
/// the top of the app's `handle_event`.
pub fn handle_event(cx: &mut Cx, event: &Event) {
    let Event::NativeLayer(event) = event else { return };
    let browsers = browsers(cx);
    let Some((&id, browser)) = browsers.map.iter_mut().find(|(_, b)| b.layer.id() == event.layer) else {
        return;
    };
    let mut error = None;
    match &event.kind {
        NativeLayerEventKind::Url(url) => browser.url.clone_from(url),
        NativeLayerEventKind::Title(title) => browser.title.clone_from(title),
        NativeLayerEventKind::Loading(loading) => browser.loading = *loading,
        NativeLayerEventKind::Crashed(message) => {
            browser.loading = false;
            error = Some(message.clone());
        }
        NativeLayerEventKind::Realized(NativeLayerMode::Unsupported) => {
            browser.loading = false;
            error = Some("web view not supported on this platform".to_string());
        }
        NativeLayerEventKind::Closed => {
            browsers.map.remove(&id);
            return;
        }
        _ => return,
    }
    let navigation = NativeSystemBrowserNavigation {
        browser_id: id.0,
        url: browser.url.clone(),
        title: browser.title.clone(),
        loading: browser.loading,
        error,
    };
    cx.action(navigation);
}
