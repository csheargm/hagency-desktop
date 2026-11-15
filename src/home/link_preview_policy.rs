//! Decides whether a link preview card is worth drawing below a message.
//!
//! A card that only repeats the host of a link already visible in the message
//! (a failed fetch such as the homeserver's 403 "URL is not allowed to be
//! previewed", a pending fetch, or metadata with no title, description or
//! image) is a large blank box, so it is not shown at all.

use super::link_preview::{LinkPreviewCacheEntry, LinkPreviewData};

/// Returns whether `entry` has something to show beyond the bare link.
pub(crate) fn is_preview_worth_showing(entry: &LinkPreviewCacheEntry) -> bool {
    match entry {
        LinkPreviewCacheEntry::LoadedLinkPreview(data) => has_preview_content(data),
        // Pending: show nothing until real metadata arrives, so a fetch that then
        // fails never flashes a placeholder card.
        LinkPreviewCacheEntry::Requested | LinkPreviewCacheEntry::Failed => false,
    }
}

fn has_preview_content(data: &LinkPreviewData) -> bool {
    let non_blank = |value: &Option<String>| value.as_deref().is_some_and(|s| !s.trim().is_empty());
    non_blank(&data.title) || non_blank(&data.description) || non_blank(&data.image)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use matrix_sdk::ruma::{api::{EndpointError, error::{Error as ClientApiError, FromHttpResponseError}}, exports::http};

    use super::*;
    use crate::home::link_preview::{insert_into_cache, TimestampedCacheEntry};
    use crate::sliding_sync::UrlPreviewError;

    fn loaded(data: LinkPreviewData) -> LinkPreviewCacheEntry {
        LinkPreviewCacheEntry::LoadedLinkPreview(data)
    }

    /// Runs a fetch result through the real cache-insertion path.
    fn entry_after_fetch(result: Result<LinkPreviewData, UrlPreviewError>) -> LinkPreviewCacheEntry {
        let slot = Arc::new(Mutex::new(TimestampedCacheEntry {
            entry: LinkPreviewCacheEntry::Requested,
            timestamp: Instant::now(),
        }));
        insert_into_cache(slot.clone(), result, None);
        let entry = slot.lock().unwrap().entry.clone();
        entry
    }

    #[test]
    fn link_preview_forbidden_by_homeserver_shows_no_card() {
        // Exactly what the beta homeserver answers for a GitHub link.
        let response = http::Response::builder()
            .status(403)
            .header("content-type", "application/json")
            .body(&br#"{"errcode":"M_FORBIDDEN","error":"URL is not allowed to be previewed"}"#[..])
            .unwrap();
        let error = FromHttpResponseError::Server(ClientApiError::from_http_response(response));
        let entry = entry_after_fetch(Err(UrlPreviewError::Request(error.into())));
        assert!(matches!(entry, LinkPreviewCacheEntry::Failed));
        assert!(!is_preview_worth_showing(&entry));
    }

    #[test]
    fn link_preview_any_failed_or_pending_fetch_shows_no_card() {
        assert!(!is_preview_worth_showing(&entry_after_fetch(Err(UrlPreviewError::ClientNotAvailable))));
        let bad_json = serde_json::from_str::<LinkPreviewData>("not json").unwrap_err();
        assert!(!is_preview_worth_showing(&entry_after_fetch(Err(UrlPreviewError::Json(bad_json)))));
        assert!(!is_preview_worth_showing(&LinkPreviewCacheEntry::Requested));
    }

    #[test]
    fn link_preview_without_title_description_or_image_shows_no_card() {
        assert!(!is_preview_worth_showing(&entry_after_fetch(Ok(LinkPreviewData::default()))));
        // A site name alone (e.g. "GitHub") repeats the host; blank strings are empty too.
        assert!(!is_preview_worth_showing(&loaded(LinkPreviewData {
            site_name: Some("GitHub".into()),
            title: Some("  ".into()),
            description: Some("\n".into()),
            ..Default::default()
        })));
    }

    #[test]
    fn link_preview_with_any_real_metadata_shows_a_card() {
        for data in [
            LinkPreviewData { title: Some("Fix mention insertion · PR #2".into()), ..Default::default() },
            LinkPreviewData { description: Some("中文 description".into()), ..Default::default() },
            LinkPreviewData { image: Some("mxc://example.org/image".into()), ..Default::default() },
        ] {
            assert!(is_preview_worth_showing(&loaded(data)));
        }
    }
}
