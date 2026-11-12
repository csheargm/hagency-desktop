//! A link preview widget that provides a method to populate link preview view for setting its' children.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use makepad_widgets::*;
use crate::{LivePtr, utils, widget_ref_from_live_ptr};
use matrix_sdk::ruma::{events::room::{ImageInfo, MediaSource}, OwnedMxcUri, UInt};
use serde::{Deserialize, Deserializer};
use url::Url;

use crate::{
    home::room_screen::TimelineUpdate,
    media_cache::MediaCache,
    shared::text_or_image::{TextOrImageRef, TextOrImageWidgetRefExt},
    shared::navigation_bar_button::NavigationBarButtonWidgetExt,
    sliding_sync::{submit_async_request, MatrixRequest, UrlPreviewError},
};

/// Maximum number of cache entries before cleanup is triggered
const MAX_CACHE_ENTRIES_BEFORE_CLEANUP: usize = 100;
/// Maximum age for cache entries in seconds (1 hour)
const CACHE_ENTRY_MAX_AGE_SECS: u64 = 3600;
/// Keep a readable title beside the thumbnail, up to three cards per row.
const MIN_PREVIEW_WIDTH: f64 = 232.0;
const PREVIEW_GAP: f64 = 12.0;
// Reserve the same space for two title lines, three summary lines and a footer,
// regardless of which metadata the website supplies. Scale with the body font.
const PREVIEW_HEIGHT: f64 = 196.0;

/// An entry in the Link Preview cache with timestamp for cleanup.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub struct TimestampedCacheEntry {
    pub entry: LinkPreviewCacheEntry,
    pub timestamp: Instant,
}

/// An entry in the Link Preview cache.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum LinkPreviewCacheEntry {
    Requested,
    LoadedLinkPreview(LinkPreviewData),
    Failed,
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.LINK_PREVIEW_MESSAGE_TEXT_STYLE = RBX_TEXT_BODY {}

    let PreviewToggle = NavigationBarButton {
        width: 36 height: 36 padding: 0 tooltip_below: true
        draw_bg +: {color_hover: RINX_HOVER color_active: RINX_SELECTED border_radius: 8}
        icon := Icon {icon_walk: Walk{width: 18 height: 18} draw_icon.color: RINX_MUTED}
    }
    mod.widgets.LinkPreview = #(LinkPreview::register_widget(vm)) {
        width: Fill{max: RINX_READING_WIDTH}, height: Fit,
        flow: Down,
        font_size: mod.widgets.MESSAGE_FONT_SIZE

        previews := View {
            width: Fill height: Fit flow: Flow.Right{wrap: true} spacing: #(PREVIEW_GAP)
        }

        collapsible_buttons := View {
            width: Fill, height: Fit,
            flow: Right,
            align: Align{x: 0.5, y: 0.5},
            padding: Inset{top: 4},
            visible: false,

            expand_button := PreviewToggle {icon.draw_icon.svg: ICON_CHEVRON_DOWN}
            collapse_button := PreviewToggle {visible: false icon.draw_icon.svg: ICON_CHEVRON_UP}
        }

        preview_template: RoundedView {
            cursor: MouseCursor.Hand
            width: Fill height: #(PREVIEW_HEIGHT) flow: Down spacing: 10
            margin: Inset{top: 8}
            padding: 12
            show_bg: true
            draw_bg +: {
                color: mod.widgets.RINX_SURFACE
                border_color: mod.widgets.RINX_BORDER
                border_size: 1.0
                border_radius: 8.0
            }
            summary := View {
                width: Fill height: Fill flow: Right spacing: 12
                content_view := View {
                    width: Fill height: Fit flow: Down spacing: 6
                    inner_content_view := View {
                        width: Fill height: Fit flow: Down
                        title_label := Label {
                            width: Fill height: Fit padding: 0
                            flow: Flow.Right{wrap: true} max_lines: 2 text_overflow: Ellipsis
                            draw_text +: {
                                text_style: RBX_TEXT_BODY_STRONG {}
                                color: mod.widgets.RINX_INK
                            }
                        }
                    }
                    description_label := Label {
                        width: Fill height: Fit padding: 0
                        flow: Flow.Right{wrap: true} max_lines: 3 text_overflow: Ellipsis
                        draw_text +: {
                            text_style: RBX_TEXT_BODY {}
                            color: mod.widgets.RINX_MUTED
                        }
                    }
                }
                image_view := View {
                    visible: false width: 72 height: 72
                    image := TextOrImage {
                        width: Fill height: Fill
                        image_view +: {
                            height: Fill
                            image +: {width: Fill height: Fill fit: ImageFit.CropToFill}
                        }
                    }
                }
            }
            footer := View {
                width: Fill height: Fit flow: Down spacing: 8
                SolidView {width: Fill height: 1 draw_bg.color: mod.widgets.RINX_BORDER}
                site_name_label := Label {
                    width: Fill height: Fit padding: 0 max_lines: 1 text_overflow: Ellipsis
                    draw_text +: {text_style: RBX_TEXT_META {} color: mod.widgets.RINX_MUTED}
                }
            }
        }
    }
}

#[derive(Script, Widget)]
pub struct LinkPreview {
    #[deref]
    view: View,
    #[live]
    preview_template: Option<LivePtr>,
    #[live]
    font_size: f32,
    #[rust]
    children: Vec<ViewRef>,
    #[rust]
    is_expanded: bool,
    #[rust]
    columns: usize,
    #[rust]
    card_width: f64,
    #[rust]
    card_height: f64,
    /// The links that were last populated in this widget, to avoid unnecessary repopulation.
    #[rust]
    last_populated_links: Vec<Url>,
    /// `clear` has left this widget empty and nothing changed it since. Messages that never
    /// show previews clear theirs on every draw; a clear that changes nothing must not ask for
    /// a redraw, because one asked for while drawing is drawn in the next frame.
    #[rust]
    cleared: bool,
}

impl ScriptHook for LinkPreview {
    fn on_after_apply(
        &mut self,
        vm: &mut ScriptVm,
        apply: &Apply,
        scope: &mut Scope,
        _value: ScriptValue,
    ) {
        // An apply can put the children back to their declared look.
        self.cleared = false;
        if apply.is_script_reapply() {
            self.card_width = 0.0;
            if let Some(template) = self.preview_template {
                for child in &mut self.children {
                    let labels: Vec<_> = [
                        ids!(title_label),
                        ids!(description_label),
                        ids!(site_name_label),
                    ]
                    .into_iter()
                    .map(|id| {
                        (
                            id,
                            child.label(vm.cx_mut(), id).text(),
                            child.label(vm.cx_mut(), id).visible(),
                        )
                    })
                    .collect();
                    let visible: Vec<_> = [ids!(image_view), ids!(footer)]
                        .into_iter()
                        .map(|id| (id, child.view(vm.cx_mut(), id).visible()))
                        .collect();
                    child.script_apply(vm, apply, scope, template);
                    for (id, text, visible) in labels {
                        let label = child.label(vm.cx_mut(), id);
                        label.set_text(vm.cx_mut(), &text);
                        label.set_visible(vm.cx_mut(), visible);
                    }
                    for (id, visible) in visible {
                        child
                            .view(vm.cx_mut(), id)
                            .set_visible(vm.cx_mut(), visible);
                    }
                }
            }
            self.update_button_and_visibility(vm.cx_mut());
        }
    }
}

impl Widget for LinkPreview {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        // clear hovers on every link preview card and the show more/fewer buttons
        if let Event::ClearHover = event {
            for item in self.children.iter() {
                reset_hover(cx, item);
            }
            self.reset_toggle_hover(cx);
        }

        // Handle collapsible button clicks
        if let Event::Actions(actions) = event {
            let expand_btn = self.view.navigation_bar_button(cx, ids!(collapsible_buttons.expand_button));
            let collapse_btn = self.view.navigation_bar_button(cx, ids!(collapsible_buttons.collapse_button));
            if expand_btn.clicked(actions) || collapse_btn.clicked(actions) {
                self.is_expanded = !self.is_expanded;
                self.update_button_and_visibility(cx);
                cx.redraw_all();
            }
        }

        for (index, view) in self.children.iter().enumerate().filter(|(_, view)| view.visible()) {
            match event.hits(cx, view.area()) {
                Hit::FingerHoverIn(_) | Hit::FingerDown(_) => {
                    let mut view = view.clone();
                    script_apply_eval!(cx, view, {
                        draw_bg.color: mod.widgets.RINX_HOVER
                    });
                }
                Hit::FingerHoverOut(_) => {
                    reset_hover(cx, view);
                }
                Hit::FingerUp(fe) => {
                    // return to normal bg color
                    reset_hover(cx, view);
                    if fe.is_over && fe.is_primary_hit() && fe.was_tap() {
                        if let Some(link) = self.last_populated_links.get(index) {
                            cx.widget_action(
                                view.widget_uid(),
                                HtmlLinkAction::Clicked {
                                    url: link.to_string(),
                                    key_modifiers: fe.modifiers,
                                },
                            );
                        }
                    }
                }
                _ => {}
            }
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let available = cx.peek_walk_turtle(walk).size.x;
        if available.is_finite() && available > 0.0 {
            let scale = (f64::from(self.font_size) / 11.0).max(1.0);
            let min_width = MIN_PREVIEW_WIDTH * scale;
            let columns = (((available + PREVIEW_GAP) / (min_width + PREVIEW_GAP)).floor() as usize)
                .clamp(1, 3);
            let card_width = ((available - PREVIEW_GAP * (columns - 1) as f64) / columns as f64).floor();
            let card_height = (PREVIEW_HEIGHT * scale).ceil();
            if self.columns != columns || self.card_width != card_width || self.card_height != card_height {
                self.columns = columns;
                self.card_width = card_width;
                self.card_height = card_height;
                for child in &self.children {
                    if let Some(mut child) = child.borrow_mut() {
                        child.walk.width = Size::Fixed(card_width);
                        child.walk.height = Size::Fixed(card_height);
                    }
                    child.redraw(cx);
                }
                self.update_button_and_visibility(cx);
                self.view.redraw(cx);
            }
        }
        // Previews are instantiated outside the message's widget tree. Carry its
        // body size explicitly, including mobile overrides and live theme reloads.
        for child in &self.children {
            for path in [ids!(title_label), ids!(description_label)] {
                if let Some(mut label) = child.label(cx, path).borrow_mut() {
                    label.draw_text.text_style.font_size = self.font_size;
                }
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

impl LinkPreview {
    fn update_button_and_visibility(&mut self, cx: &mut Cx) {
        for (index, child) in self.children.iter().enumerate() {
            child.set_visible(cx, self.is_expanded || index < self.columns.max(1));
        }
        let hidden = self.children.len().saturating_sub(self.columns.max(1));
        if hidden > 0 {
            self.view.view(cx, ids!(collapsible_buttons)).set_visible(cx, true);
            let expand_btn = self.view.navigation_bar_button(cx, ids!(collapsible_buttons.expand_button));
            let collapse_btn = self.view.navigation_bar_button(cx, ids!(collapsible_buttons.collapse_button));
            if self.is_expanded {
                expand_btn.set_visible(cx, false);
                collapse_btn.set_visible(cx, true);
            } else {
                if let Some(mut button) = expand_btn.borrow_mut() {
                    button.set_tooltip_text(format!("{} ({hidden})", crate::i18n::tr("Show more links")));
                }
                expand_btn.set_visible(cx, true);
                collapse_btn.set_visible(cx, false);
            }
            if let Some(mut button) = collapse_btn.borrow_mut() {
                button.set_tooltip_text(crate::i18n::tr("Show fewer links"));
            }
            self.reset_toggle_hover(cx);
        } else {
            self.view.view(cx, ids!(collapsible_buttons)).set_visible(cx, false);
        }
    }

    fn reset_toggle_hover(&self, cx: &mut Cx) {
        for path in [ids!(collapsible_buttons.expand_button), ids!(collapsible_buttons.collapse_button)] {
            if let Some(mut button) = self.view.navigation_bar_button(cx, path).borrow_mut() {
                button.animator_play(cx, ids!(hover.off));
                cx.widget_action(button.widget_uid(), TooltipAction::HoverOut);
            }
        }
    }

    fn sync_children(&mut self, cx: &mut Cx) {
        // Cards must belong to the view's layout and widget tree. Drawing them
        // before the view left its area covering only the expand/collapse button.
        if let Some(mut previews) = self.view.view(cx, ids!(previews)).borrow_mut() {
            previews.children = self.children.iter().enumerate()
                .map(|(index, child)| (LiveId::from_num(id!(preview).0, index as u64), WidgetRef::clone(child)))
                .collect();
            cx.widget_tree_mark_dirty(previews.widget_uid());
            previews.redraw(cx);
        }
    }
}

impl LinkPreviewRef {
    /// Clears any displayed link preview(s), resetting this widget to its empty state.
    ///
    /// Needed for messages that never show link previews (e.g. redacted messages).
    pub fn clear(&mut self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() {
            if inner.cleared {
                return;
            }
            inner.children.clear();
            inner.sync_children(cx);
            inner.last_populated_links.clear();
            inner.is_expanded = false;
            inner.columns = 0;
            inner.card_width = 0.0;
            inner.update_button_and_visibility(cx);
            inner.redraw(cx);
            inner.cleared = true;
        }
    }

    /// Populates the link previews below a message.
    ///
    /// Returns `true` if every preview was fully drawn.
    pub fn populate_below_message<F>(
        &mut self,
        cx: &mut Cx,
        links: &[url::Url],
        media_cache: &mut MediaCache,
        link_preview_cache: &mut LinkPreviewCache,
        populate_image_fn: &F,
    ) -> bool
    where
        F: Fn(&mut Cx, &TextOrImageRef, Option<&ImageInfo>, MediaSource, &str, &mut MediaCache) -> bool,
    {
        let accepted_links = previewable_links(links);

        let did_links_change = match self.borrow() {
            Some(inner) => inner.last_populated_links != accepted_links,
            None => return true,
        };
        if did_links_change {
            if let Some(mut inner) = self.borrow_mut() {
                let num_links = accepted_links.len();
                inner.cleared = false;
                // Reuse as many old link preview child instances as we can.
                inner.children.truncate(num_links);
                while inner.children.len() < num_links {
                    let child = widget_ref_from_live_ptr(cx, inner.preview_template).as_view();
                    inner.children.push(child);
                }
                // Reset each preview's per-link visual state: a reused one keeps its old
                // hover color and image, and a new one shows TextOrImage's default view.
                for item in inner.children.iter() {
                    reset_hover(cx, item);
                    item.text_or_image(cx, ids!(image)).clear(cx);
                }
                inner.last_populated_links = accepted_links;
                inner.is_expanded = false;
                // Newly added children must get the current grid dimensions even
                // when the available width and column count did not change.
                inner.card_width = 0.0;
                inner.sync_children(cx);
                inner.update_button_and_visibility(cx);
            }
        }

        let Some(inner) = self.borrow() else { return true };
        let mut all_drawn = true;
        for (view, link) in inner.children.iter().zip(inner.last_populated_links.iter()) {
            let entry = link_preview_cache.get_or_fetch_link_preview(link.as_str());
            all_drawn &= populate_preview_item(cx, view, entry, link, media_cache, populate_image_fn);
        }
        all_drawn
    }
}

fn previewable_links(links: &[Url]) -> Vec<Url> {
    let mut accepted = Vec::new();
    for link in links {
        if !matches!(link.scheme(), "http" | "https") || link.host_str().is_none()
            || link.host_str().is_some_and(|host|
                ["matrix.to", "matrix.io"].iter().any(|skip|
                    host == *skip || host.strip_suffix(skip).is_some_and(|prefix| prefix.ends_with('.'))
                )
            )
        {
            continue;
        }
        if !accepted.contains(link) {
            accepted.push(link.clone());
        }
    }
    accepted
}

fn reset_hover(cx: &mut Cx, item: &ViewRef) {
    let mut item = item.clone();
    script_apply_eval!(cx, item, {
        draw_bg.color: mod.widgets.RINX_SURFACE
    });
}

/// Populates a single link preview with whatever metadata has been fetched so far.
///
/// Returns `true` if the link preview was fully drawn.
fn populate_preview_item<F>(
    cx: &mut Cx,
    view: &ViewRef,
    entry: LinkPreviewCacheEntry,
    link: &Url,
    media_cache: &mut MediaCache,
    populate_image_fn: &F,
) -> bool
where
    F: Fn(&mut Cx, &TextOrImageRef, Option<&ImageInfo>, MediaSource, &str, &mut MediaCache) -> bool,
{
    let title_link = view.label(cx, ids!(title_label));
    let site_name_label = view.label(cx, ids!(site_name_label));
    let description_label = view.label(cx, ids!(description_label));
    let image_view = view.view(cx, ids!(image_view));
    let text_or_image = view.text_or_image(cx, ids!(image));

    let host = link.host_str().unwrap_or_default();
    let footer = view.view(cx, ids!(footer));
    let description = |value: &str| utils::replace_linebreaks_separators(value, false).trim().to_owned();
    let data = match entry {
        LinkPreviewCacheEntry::LoadedLinkPreview(data) => data,
        LinkPreviewCacheEntry::Requested | LinkPreviewCacheEntry::Failed => {
            title_link.set_text(cx, host);
            description_label.set_text(cx, if matches!(entry, LinkPreviewCacheEntry::Requested) {
                crate::i18n::tr("Loading preview…")
            } else {
                crate::i18n::tr("Open webpage")
            });
            description_label.set_visible(cx, true);
            footer.set_visible(cx, false);
            image_view.set_visible(cx, false);
            return matches!(entry, LinkPreviewCacheEntry::Failed);
        }
    };

    let title = data.title.as_deref().map(description).filter(|s| !s.is_empty());
    title_link.set_text(cx, title.as_deref().unwrap_or(host));
    let body = data.description.as_deref().map(description).filter(|s| !s.is_empty());
    description_label.set_text(cx, body.as_deref().unwrap_or(""));
    description_label.set_visible(cx, body.is_some());
    let site = data.site_name.as_deref().map(description).filter(|s| !s.is_empty());
    site_name_label.set_text(cx, &match site {
        Some(site) if !site.eq_ignore_ascii_case(host) => format!("{site} · {host}"),
        _ => host.to_owned(),
    });
    footer.set_visible(cx, title.is_some() || body.is_some() || data.image.is_some());

    let Some(image) = &data.image else {
        image_view.set_visible(cx, false);
        return true;
    };
    let mut image_info = ImageInfo::default();
    image_info.mimetype = data.image_type.clone();
    image_info.size = data.image_size;
    let source = MediaSource::Plain(OwnedMxcUri::from(image.clone()));
    let fully_drawn = populate_image_fn(cx, &text_or_image, Some(&image_info), source, "", media_cache);
    // Only show the image area once there's an actual image in it. Anything else means
    // it's still loading or couldn't be fetched, and an error message doesn't belong here.
    image_view.set_visible(cx, text_or_image.status().is_image());
    fully_drawn
}

/// The data structure from the link preview API, "/_matrix/client/v1/media/preview_url"
#[derive(Clone, Debug, Deserialize, Default)]
pub struct LinkPreviewData {
    #[serde(rename = "og:description")]
    pub description: Option<String>,
    /// The size of the image in bytes, if available
    #[serde(rename = "matrix:image:size", default, deserialize_with = "deserialize_lenient_uint")]
    pub image_size: Option<UInt>,
    /// The URL of the image
    #[serde(rename = "og:image")]
    pub image: Option<String>,
    /// The type of the image
    #[serde(rename = "og:image:type")]
    pub image_type: Option<String>,
    /// The name of the site
    #[serde(rename = "og:site_name")]
    pub site_name: Option<String>,
    /// The URL of the site
    #[serde(rename = "og:url")]
    pub url: Option<String>,
    /// The title of the site
    #[serde(rename = "og:title")]
    pub title: Option<String>,
}

/// Deserializes an optional [`UInt`] that is either a JSON number or a JSON string.
///
/// Some homeservers encode the numeric preview fields as strings, so we handle both.
fn deserialize_lenient_uint<'de, D>(deserializer: D) -> Result<Option<UInt>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumberOrString {
        Number(UInt),
        String(String),
    }
    Ok(match Option::<NumberOrString>::deserialize(deserializer)? {
        Some(NumberOrString::Number(n)) => Some(n),
        Some(NumberOrString::String(s)) => s.parse::<u64>().ok().and_then(|n| UInt::try_from(n).ok()),
        None => None,
    })
}

/// The cache for link previews, specific to a given timeline.
pub struct LinkPreviewCache {
    /// The actual cached data.
    cache: BTreeMap<String, Arc<Mutex<TimestampedCacheEntry>>>,
    /// A channel to send updates to a particular timeline when a link preview request has completed.
    timeline_update_sender: Option<crate::timeline_updates::Sender>,
}

impl LinkPreviewCache {
    pub fn clear(&mut self) { self.cache.clear(); }

    /// Creates a new link preview cache that will optionally send updates
    /// when a link preview request has completed.
    pub const fn new(
        timeline_update_sender: Option<crate::timeline_updates::Sender>,
    ) -> Self {
        Self {
            cache: BTreeMap::new(),
            timeline_update_sender,
        }
    }

    /// Cache metadata that has already been fetched, without requesting it again.
    pub fn insert(&mut self, url: &Url, data: LinkPreviewData) {
        self.cache.insert(url.to_string(), Arc::new(Mutex::new(TimestampedCacheEntry {
            entry: LinkPreviewCacheEntry::LoadedLinkPreview(data),
            timestamp: Instant::now(),
        })));
    }

    /// Sets a new timeline update sender, e.g., for when the backend re-created this room's timeline.
    pub fn set_timeline_update_sender(
        &mut self,
        sender: crate::timeline_updates::Sender,
    ) {
        self.timeline_update_sender = Some(sender);
    }

    /// Fetches the link preview for the specified URL.
    pub fn get_or_fetch_link_preview(&mut self, url: &str) -> LinkPreviewCacheEntry {
        if self.cache.len() >= 128 && !self.cache.contains_key(url) {
            let oldest = self.cache.iter().filter_map(|(url, entry)| {
                let entry = entry.lock().ok()?;
                if matches!(entry.entry, LinkPreviewCacheEntry::Requested) {None} else {Some((url.clone(), entry.timestamp))}
            }).min_by_key(|(_, time)| *time).map(|(url, _)| url);
            if let Some(oldest) = oldest { self.cache.remove(&oldest); }
            else { return LinkPreviewCacheEntry::Failed; }
        }
        // Clean up old entries periodically
        if self.cache.len() > MAX_CACHE_ENTRIES_BEFORE_CLEANUP {
            self.cleanup_old_entries(Duration::from_secs(CACHE_ENTRY_MAX_AGE_SECS));
        }

        if let Some(entry) = self.cache.get(url) {
            return entry.lock().unwrap().entry.clone();
        }
        let entry_ref = Arc::new(Mutex::new(TimestampedCacheEntry {
            entry: LinkPreviewCacheEntry::Requested,
            timestamp: Instant::now(),
        }));
        self.cache.insert(url.to_owned(), entry_ref.clone());
        submit_async_request(MatrixRequest::GetUrlPreview {
            url: url.to_owned(),
            on_fetched: insert_into_cache,
            destination: entry_ref,
            update_sender: self.timeline_update_sender.clone(),
        });
        LinkPreviewCacheEntry::Requested
    }

    /// Removes all `Requested` and `Failed` entries from the link preview cache,
    /// allowing them to be re-fetched.
    ///
    /// This should be called when the app transitions from offline back to online,
    /// because any in-flight requests that were submitted while offline have likely
    /// failed, leaving stale entries that permanently block re-fetching.
    pub fn clear_all_pending_and_failed_requests(&mut self) {
        self.cache.retain(|_, entry| {
            if let Ok(guard) = entry.lock() {
                matches!(guard.entry, LinkPreviewCacheEntry::LoadedLinkPreview(_))
            } else {
                true // Keep entries we can't lock
            }
        });
    }

    /// Removes cache entries older than the specified duration
    pub fn cleanup_old_entries(&mut self, max_age: Duration) {
        let now = Instant::now();
        self.cache.retain(|_url, entry| {
            if let Ok(timestamped_entry) = entry.lock() {
                now.duration_since(timestamped_entry.timestamp) < max_age
            } else {
                true // Keep entries we can't lock
            }
        });
    }
}

/// Insert data into a previously-requested media cache entry.
fn insert_into_cache(
    value_ref: Arc<Mutex<TimestampedCacheEntry>>,
    data: Result<LinkPreviewData, UrlPreviewError>,
    update_sender: Option<crate::timeline_updates::Sender>,
) {
    let new_entry = match data {
        Ok(data) => LinkPreviewCacheEntry::LoadedLinkPreview(data),
        Err(e) => {
            error!("Homeserver link preview failed: {e}");
            LinkPreviewCacheEntry::Failed
        }
    };

    if let Ok(mut timestamped_entry) = value_ref.lock() {
        timestamped_entry.entry = new_entry;
        timestamped_entry.timestamp = Instant::now();
    }

    if let Some(sender) = update_sender {
        // Reuse TimelineUpdate MediaFetched to trigger redraw in the timeline.
        let _ = sender.send(TimelineUpdate::LinkPreviewFetched);
    }
    SignalToUI::set_ui_signal();
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;

    fn draw_preview(cx: &mut Cx, preview: &mut LinkPreview, width: f64) {
        cx.redraw_all();
        let pass = DrawPass::new(cx);
        pass.set_size(cx, dvec2(width, 1800.0));
        let mut list = DrawList2d::new(cx);
        let event = DrawEvent {redraw_all: true, ..DrawEvent::default()};
        let mut draw = CxDraw::new(cx, &event);
        let mut cx2d = Cx2d::new(&mut draw);
        cx2d.begin_pass(&pass, None); list.begin_always(&mut cx2d);
        cx2d.begin_root_turtle(dvec2(width, 1800.0), Layout::flow_down());
        preview.draw_walk_all(&mut cx2d, &mut Scope::empty(), Walk {
            width: Size::Fixed(width), height: Size::Fit {min: None, max: None}, ..Walk::default()
        });
        cx2d.end_pass_sized_turtle(); list.end(&mut cx2d); cx2d.end_pass(&pass);
    }

    #[test]
    fn responsive_cards_keep_one_collapsed_row_and_reflow_expanded_links_without_recycling() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let widget = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm); crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.LinkPreview {}});
            WidgetRef::script_from_value(vm, value)
        });
        let mut preview_ref = widget.as_link_preview();
        let links: Vec<Url> = (0..7).map(|i| format!("https://example.org/{i}").parse().unwrap()).collect();
        let mut cache = LinkPreviewCache::new(None);
        for (i, link) in links.iter().enumerate() {
            cache.insert(link, LinkPreviewData {
                title: Some(format!("Article {i} {}", "Long title 中文 ".repeat(i % 3))),
                description: (i % 3 != 0).then(|| "A readable preview description 中文 ".repeat(i % 3 + 1)),
                site_name: (i % 2 == 0).then(|| "example.org".into()),
                ..Default::default()
            });
        }
        preview_ref.populate_below_message(&mut cx, &links, &mut MediaCache::new(None), &mut cache, &|_,_,_,_,_,_| true);
        let mut preview = preview_ref.borrow_mut().unwrap();
        let identities: Vec<_> = preview.children.iter().map(|c| c.widget_uid()).collect();
        for expanded in [false, true, false] {
            preview.is_expanded = expanded;
            preview.update_button_and_visibility(&mut cx);
            for (width, columns) in [(760.0, 3), (500.0, 2), (240.0, 1), (760.0, 3)] {
                draw_preview(&mut cx, &mut preview, width);
                let visible: Vec<_> = preview.children.iter().filter(|c| c.visible()).map(|c| c.area().rect(&cx)).collect();
                assert_eq!(visible.len(), if expanded {7} else {columns});
                for (index, rect) in visible.iter().enumerate() {
                    assert!(rect.size.x > 0.0 && rect.pos.x >= 0.0 && rect.pos.x + rect.size.x <= width + 0.1, "{rect:?}");
                    assert_eq!(rect.size, visible[0].size, "metadata must not change card size");
                    let row_start = index / columns * columns;
                    assert!((rect.pos.y - visible[row_start].pos.y).abs() < 0.1);
                    if index % columns > 0 {
                        let previous = visible[index - 1];
                        assert!(rect.pos.x >= previous.pos.x + previous.size.x + PREVIEW_GAP - 0.1);
                    }
                    if row_start > 0 {
                        let bottom = visible[row_start - columns..row_start].iter().map(|r| r.pos.y + r.size.y).fold(0.0, f64::max);
                        assert!(rect.pos.y >= bottom, "rows must not overlap");
                    }
                    let container = preview.view.area().rect(&cx);
                    assert!(rect.pos.y + rect.size.y <= container.pos.y + container.size.y + 0.1);
                }
                assert_eq!(preview.children.iter().map(|c| c.widget_uid()).collect::<Vec<_>>(), identities);
                assert_eq!(preview.last_populated_links, links);
            }
        }
        let grid_size = preview.children[0].area().rect(&cx).size;
        drop(preview);
        // Reusing a row for fewer links must not stretch its card, and adding
        // links again at the same width must size every new child.
        for count in [1, 7] {
            preview_ref.populate_below_message(&mut cx, &links[..count], &mut MediaCache::new(None), &mut cache, &|_,_,_,_,_,_| true);
            let mut preview = preview_ref.borrow_mut().unwrap();
            draw_preview(&mut cx, &mut preview, 760.0);
            for child in preview.children.iter().filter(|child| child.visible()) {
                assert_eq!(child.area().rect(&cx).size, grid_size);
            }
        }
        let mut preview = preview_ref.borrow_mut().unwrap();
        preview.font_size = 16.5;
        draw_preview(&mut cx, &mut preview, 760.0);
        assert_eq!(preview.columns, 2);
        for child in preview.children.iter().filter(|child| child.visible()) {
            let card = child.area().rect(&cx);
            assert_eq!(card.size.y, PREVIEW_HEIGHT * 1.5);
            for path in [ids!(title_label), ids!(description_label), ids!(site_name_label)] {
                let label = child.label(&mut cx, path);
                if label.visible() {
                    let text = label.area().rect(&cx);
                    assert!(text.pos.y + text.size.y <= card.pos.y + card.size.y - 11.0, "{text:?} outside {card:?}");
                }
            }
        }
    }

    #[test]
    fn preview_urls_preserve_order_and_only_skip_actual_matrix_domains() {
        let links: Vec<Url> = [
            "https://example.org/article", "mailto:reader@example.org", "ftp://example.org/file",
            "https://matrix.to/#/@reader:example.org", "https://sub.matrix.io/room",
            "http://example.org/page", "https://example.org/article", "https://notmatrix.to/article",
            "matrix:u/reader:example.org", "file:///tmp/article.html",
        ].into_iter().map(|s| s.parse().unwrap()).collect();
        assert_eq!(previewable_links(&links).iter().map(Url::as_str).collect::<Vec<_>>(), vec![
            "https://example.org/article", "http://example.org/page", "https://notmatrix.to/article",
        ]);
    }

    #[test]
    fn homeserver_metadata_accepts_string_image_sizes_and_missing_fields() {
        let data: LinkPreviewData = serde_json::from_value(serde_json::json!({
            "og:title": "An article", "og:description": "中文 description",
            "og:image": "mxc://example.org/image", "matrix:image:size": "1234",
        })).unwrap();
        assert_eq!(data.title.as_deref(), Some("An article"));
        assert_eq!(data.description.as_deref(), Some("中文 description"));
        assert_eq!(data.image_size.map(u64::from), Some(1234));
        let empty: LinkPreviewData = serde_json::from_str("{}").unwrap();
        assert!(empty.title.is_none() && empty.image.is_none());
    }
}
