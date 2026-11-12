use std::{ops::Deref, sync::{Arc, Mutex}};
use hashbrown::{hash_map::RawEntryMut, HashMap};
use makepad_widgets::{error, SignalToUI};
use matrix_sdk::{media::{MediaFormat, MediaRequestParameters, MediaThumbnailSettings}, ruma::{events::room::MediaSource, OwnedMxcUri}, Error, HttpError};
use matrix_sdk::reqwest::StatusCode;
use crate::{home::room_screen::TimelineUpdate, shared::attachment_download::media_source_mxc, sliding_sync::{self, MatrixRequest}};

/// The value type in the media cache, one per Matrix URI.
#[derive(Debug, Clone)]
pub struct MediaCacheValue {
    full_file: Option<MediaCacheEntryRef>,
    thumbnail: Option<(MediaCacheEntryRef, MediaThumbnailSettings)>,
    last_used: u64,
}

/// An entry in the media cache.
#[derive(Debug, Clone)]
pub enum MediaCacheEntry {
    /// A request has been issued and we're waiting for it to complete.
    Requested,
    /// The media has been successfully loaded from the server.
    Loaded(Arc<[u8]>),
    /// The media failed to load from the server with reqwest status code.
    Failed(StatusCode),
}

/// A reference to a media cache entry and its associated format.
pub type MediaCacheEntryRef = Arc<Mutex<MediaCacheEntry>>;


/// A cache of fetched media, indexed by Matrix URI.
///
/// A single Matrix URI may have multiple media formats associated with it,
/// such as a thumbnail and a full-size image.
pub struct MediaCache {
    /// The actual cached data.
    /// We use `hashbrown::HashMap` to enable the `raw_entry` API, which allows
    /// looking up entries by reference (`&OwnedMxcUri`) without cloning the key.
    cache: HashMap<OwnedMxcUri, MediaCacheValue>,
    /// A channel to send updates to a particular timeline when a media request has completed.
    timeline_update_sender: Option<crate::timeline_updates::Sender>,
    clock: u64,
}
impl Deref for MediaCache {
    type Target = HashMap<OwnedMxcUri, MediaCacheValue>;
    fn deref(&self) -> &Self::Target {
        &self.cache
    }
}

impl MediaCache {
    /// Creates a new media cache that will use the given media format
    /// when fetching media from the server.
    ///
    /// It will also optionally send updates to the given timeline update sender
    /// when a media request has completed.
    pub fn new(
        timeline_update_sender: Option<crate::timeline_updates::Sender>,
    ) -> Self {
        Self {
            cache: HashMap::new(),
            timeline_update_sender,
            clock: 0,
        }
    }

    pub fn timeline_update_sender(&self) -> Option<&crate::timeline_updates::Sender> {
        self.timeline_update_sender.as_ref()
    }

    /// Sets a new timeline update sender for this media cache's timeline.
    pub fn set_timeline_update_sender(
        &mut self,
        sender: crate::timeline_updates::Sender,
    ) {
        self.timeline_update_sender = Some(sender);
    }

    /// Tries to get the media from the cache, or submits an async request to fetch it.
    ///
    /// This method *does not* block or wait for the media to be fetched,
    /// and will return `MediaCache::Requested` while the async request is in flight.
    /// If a request is already in flight, this will not issue a new redundant request.
    ///
    /// * If the `media_format` is requesting a thumbnail that is not yet in the cache,
    ///   this function will fetch the thumbnail, and return the full-size image (if it exists).
    /// * If the `media_format` is requesting a full-size image that is not yet in the cache,
    ///   this function will fetch the full-size image, and return a thumbnail (if it exists).
    ///
    /// Returns a tuple of the media cache entry and the media format of that cached entry.
    pub fn try_get_media_or_fetch(
        &mut self,
        source: &MediaSource,
        requested_format: MediaFormat,
    ) -> (MediaCacheEntry, MediaFormat) {
        let mxc_uri = media_source_mxc(source);
        self.clock = self.clock.saturating_add(1);
        if let Some(value) = self.cache.get_mut(mxc_uri) {
            value.last_used = self.clock;
        }
        self.trim(Some(mxc_uri));
        let mut post_request_retval = (MediaCacheEntry::Requested, requested_format.clone());
        let entry_ref_to_fetch: MediaCacheEntryRef;

        if !self.cache.contains_key(mxc_uri) && self.cache.len() >= 128 {
            return post_request_retval;
        }
        let entry = self.cache.raw_entry_mut().from_key(mxc_uri);

        match entry {
            RawEntryMut::Occupied(mut occupied) => {
                let value = occupied.get_mut();
                match requested_format {
                    MediaFormat::Thumbnail(ref requested_mts) => {
                        if let Some((entry_ref, existing_mts)) = value.thumbnail.as_ref() {
                            return (
                                entry_ref.lock().unwrap().deref().clone(),
                                MediaFormat::Thumbnail(existing_mts.clone()),
                            );
                        } else {
                            // Here, a thumbnail was requested but not found, so fetch it.
                            let entry_ref = Arc::new(Mutex::new(MediaCacheEntry::Requested));
                            value.thumbnail = Some((Arc::clone(&entry_ref), requested_mts.clone()));
                            // If a full-size image is already loaded, return it.
                            if let Some(existing_file) = value.full_file.as_ref() {
                                if let MediaCacheEntry::Loaded(d) = existing_file.lock().unwrap().deref() {
                                    post_request_retval = (
                                        MediaCacheEntry::Loaded(Arc::clone(d)),
                                        MediaFormat::File,
                                    );
                                }
                            }
                            entry_ref_to_fetch = entry_ref;
                        }
                    }
                    MediaFormat::File => {
                        if let Some(entry_ref) = value.full_file.as_ref() {
                            return (
                                entry_ref.lock().unwrap().deref().clone(),
                                MediaFormat::File,
                            );
                        } else {
                            // Here, a full-size image was requested but not found, so fetch it.
                            let entry_ref = Arc::new(Mutex::new(MediaCacheEntry::Requested));
                            value.full_file = Some(entry_ref.clone());
                            // If a thumbnail is already loaded, return it.
                            if let Some((existing_thumbnail, existing_mts)) = value.thumbnail.as_ref() {
                                if let MediaCacheEntry::Loaded(d) = existing_thumbnail.lock().unwrap().deref() {
                                    post_request_retval = (
                                        MediaCacheEntry::Loaded(Arc::clone(d)),
                                        MediaFormat::Thumbnail(existing_mts.clone()),
                                    );
                                }
                            }
                            entry_ref_to_fetch = entry_ref;
                        }
                    }
                }
            }
            RawEntryMut::Vacant(vacant) => {
                let entry_ref = Arc::new(Mutex::new(MediaCacheEntry::Requested));
                let value = match &requested_format {
                    MediaFormat::Thumbnail(requested_mts) => MediaCacheValue {
                        full_file: None,
                        thumbnail: Some((Arc::clone(&entry_ref), requested_mts.clone())),
                        last_used: self.clock,
                    },
                    MediaFormat::File => MediaCacheValue {
                        full_file: Some(Arc::clone(&entry_ref)),
                        thumbnail: None,
                        last_used: self.clock,
                    },
                };
                vacant.insert(mxc_uri.clone(), value);
                entry_ref_to_fetch = entry_ref;
            }
        }

        sliding_sync::submit_async_request(MatrixRequest::FetchMedia {
            media_request: MediaRequestParameters {
                source: source.clone(),
                format: requested_format,
            },
            on_fetched: insert_into_cache,
            destination: entry_ref_to_fetch,
            update_sender: self.timeline_update_sender.clone(),
        });
        post_request_retval
    }

    /// Removes all `Requested` and `Failed` entries from the media cache,
    /// allowing them to be re-fetched.
    ///
    /// This should be called when the app transitions from offline back to online,
    /// because any in-flight requests that were submitted while offline have likely
    /// failed, leaving stale entries that permanently block re-fetching.
    pub fn clear_all_pending_and_failed_requests(&mut self) {
        self.cache.retain(|_, value| {
            // Remove `Requested`/`Failed` sub-entries, keeping `Loaded` ones.
            if let Some(ref entry_ref) = value.full_file {
                if !matches!(*entry_ref.lock().unwrap(), MediaCacheEntry::Loaded(_)) {
                    value.full_file = None;
                }
            }
            if let Some((ref entry_ref, _)) = value.thumbnail {
                if !matches!(*entry_ref.lock().unwrap(), MediaCacheEntry::Loaded(_)) {
                    value.thumbnail = None;
                }
            }
            // Keep the top-level entry only if at least one sub-entry remains.
            value.full_file.is_some() || value.thumbnail.is_some()
        });
    }

    /// Drop old completed media; leave in-flight entries in place to deduplicate requests.
    /// The current asset is pinned for this draw, including a single oversized asset.
    pub fn trim(&mut self, pinned: Option<&OwnedMxcUri>) {
        const MAX_BYTES: usize = 32 * 1024 * 1024;
        const MAX_ENTRIES: usize = 128;
        let mut bytes = 0usize;
        let mut candidates = Vec::new();
        for (uri, value) in &self.cache {
            let mut size = 0usize;
            let mut pending = false;
            for entry in value.full_file.iter().chain(value.thumbnail.iter().map(|(entry, _)| entry)) {
                match &*entry.lock().unwrap() {
                    MediaCacheEntry::Loaded(data) => size = size.saturating_add(data.len()),
                    MediaCacheEntry::Requested => pending = true,
                    MediaCacheEntry::Failed(_) => {},
                }
            }
            bytes = bytes.saturating_add(size);
            if !pending && pinned != Some(uri) { candidates.push((value.last_used, uri.clone(), size)); }
        }
        candidates.sort_unstable_by_key(|(used, _, _)| *used);
        for (_, uri, size) in candidates {
            if bytes <= MAX_BYTES && self.cache.len() <= MAX_ENTRIES { break }
            self.cache.remove(&uri);
            bytes = bytes.saturating_sub(size);
        }
    }

    pub fn clear(&mut self) { self.cache.clear(); }

}

/// Converts a Matrix SDK error to a MediaCacheEntry::Failed with appropriate status codes.
pub(crate) fn error_to_media_cache_entry(error: Error, _request: &MediaRequestParameters) -> MediaCacheEntry {
    match error {
        Error::Http(http_error) => {
            // Sometimes http errors are wrapped in the `Cached` variant.
            let mut http_error: &HttpError = &http_error;
            while let HttpError::Cached(cached) = http_error {
                http_error = cached;
            }
            if let Some(client_error) = http_error.as_client_api_error() {
                error!("Media request failed with HTTP {}", client_error.status_code);
                MediaCacheEntry::Failed(client_error.status_code)
            } else {
                match http_error {
                    HttpError::Reqwest(reqwest_error) => {
                        // Checking if the connection is timeout is not important as Matrix SDK has implemented maximum timeout duration.
                        if reqwest_error.is_status() {
                            MediaCacheEntry::Failed(reqwest_error
                                .status()
                                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
                        } else {
                            MediaCacheEntry::Failed(StatusCode::INTERNAL_SERVER_ERROR)
                        }
                    }
                    _ => MediaCacheEntry::Failed(StatusCode::NOT_FOUND),
                }
            }
        }
        Error::UnknownError(error) if error.is::<crate::media_transport::PreviewLimit>() => MediaCacheEntry::Failed(
            if matches!(error.downcast_ref(),Some(crate::media_transport::PreviewLimit::TooLarge)) {StatusCode::PAYLOAD_TOO_LARGE} else {StatusCode::SERVICE_UNAVAILABLE}),
        Error::InsufficientData => MediaCacheEntry::Failed(StatusCode::PARTIAL_CONTENT),
        Error::AuthenticationRequired => MediaCacheEntry::Failed(StatusCode::UNAUTHORIZED),
        _ => MediaCacheEntry::Failed(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

/// Insert data into a previously-requested media cache entry.
fn insert_into_cache<D: Into<Arc<[u8]>>>(
    value_ref: &Mutex<MediaCacheEntry>,
    request: MediaRequestParameters,
    data: matrix_sdk::Result<D>,
    update_sender: Option<crate::timeline_updates::Sender>,
    scope: MediaFetchScope,
) {
    if !scope.is_current() { return; }
    let new_value = match data {
        Ok(data) => {
            let data = data.into();
            if crate::memory_budget::admit_encoded(&data) {
                MediaCacheEntry::Loaded(data)
            } else { MediaCacheEntry::Failed(StatusCode::PAYLOAD_TOO_LARGE) }
        }
        Err(e) => error_to_media_cache_entry(e, &request)
    };

    *value_ref.lock().unwrap() = new_value;

    if let Some(sender) = update_sender {
        let _ = sender.send(TimelineUpdate::MediaFetched(request));
    }
    SignalToUI::set_ui_signal();
}

#[cfg(test)]
mod tests {
    use super::*;
    fn uri(n: usize) -> OwnedMxcUri { format!("mxc://example.org/{n}").into() }
    fn loaded(bytes: usize, used: u64) -> MediaCacheValue {
        MediaCacheValue { full_file: Some(Arc::new(Mutex::new(MediaCacheEntry::Loaded(vec![0; bytes].into())))),
            thumbnail: None, last_used: used }
    }
    #[test]
    fn byte_budget_evicts_old_media_and_pins_the_current_asset() {
        let mut cache = MediaCache::new(None);
        cache.cache.insert(uri(1), loaded(20 * 1024 * 1024, 1));
        cache.cache.insert(uri(2), loaded(20 * 1024 * 1024, 2));
        cache.trim(Some(&uri(2)));
        assert!(!cache.cache.contains_key(&uri(1)));
        assert!(cache.cache.contains_key(&uri(2)));
    }
    #[test]
    fn pending_media_stays_deduplicated_and_completed_entries_are_bounded() {
        let mut cache = MediaCache::new(None);
        for n in 0..140 { cache.cache.insert(uri(n), loaded(1, n as u64)); }
        let pending = Arc::new(Mutex::new(MediaCacheEntry::Requested));
        cache.cache.insert(uri(200), MediaCacheValue { full_file: Some(pending.clone()), thumbnail: None, last_used: 0 });
        cache.trim(None);
        assert_eq!(cache.cache.len(), 128);
        assert!(Arc::ptr_eq(cache.cache[&uri(200)].full_file.as_ref().unwrap(), &pending));
        let source = MediaSource::Plain(uri(200));
        assert!(matches!(cache.try_get_media_or_fetch(&source, MediaFormat::File).0, MediaCacheEntry::Requested));
    }
}

/// Admission and UI delivery belong to the original account and SDK instance.
#[derive(Clone, Copy, Debug)]
pub struct MediaFetchScope { epoch: u64, revision: u64 }
impl MediaFetchScope {
    pub(crate) fn capture() -> Self { Self {epoch: crate::account_session::epoch(), revision: crate::sliding_sync::client_revision()} }
    #[cfg(test)]
    pub(crate) fn stale_for_test() -> Self {Self {epoch: u64::MAX, revision: u64::MAX}}
    pub fn is_current(&self) -> bool { !crate::account_session::is_closing() && !crate::logout::logout_state_machine::is_logout_in_progress() && self.epoch == crate::account_session::epoch() && self.revision == crate::sliding_sync::client_revision() }
    pub(crate) fn cache_key(&self, original: &std::path::Path) -> std::path::PathBuf {
        let encoded: String = url::form_urlencoded::byte_serialize(original.as_os_str().as_encoded_bytes()).collect();
        format!("chat-media/{}/{}/{}", self.epoch, self.revision, encoded).into()
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn image_cache_keys_never_cross_account_or_sdk_instances_or_lose_absolute_path_namespace() {
        let path = std::path::Path::new("/private/image.png");
        let first = MediaFetchScope {epoch:1,revision:2}.cache_key(path);
        assert!(first.starts_with("chat-media/1/2"));
        assert_ne!(first, MediaFetchScope {epoch:2,revision:2}.cache_key(path));
        assert_ne!(first, MediaFetchScope {epoch:1,revision:3}.cache_key(path));
        assert!(!MediaFetchScope::stale_for_test().is_current());
    }
}
