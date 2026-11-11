use std::{cell::RefCell, sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering}}};
use indexmap::IndexMap;
use crossbeam_queue::ArrayQueue;
use makepad_widgets::{Cx, SignalToUI};
use matrix_sdk::ruma::OwnedMxcUri;

use crate::sliding_sync::{submit_async_request, MatrixRequest};


thread_local! {
    /// A cache of Avatar images, indexed by Matrix URI.
    ///
    /// To be of any use, this cache must only be accessed by the main UI thread.
    static AVATAR_NEW_CACHE: RefCell<IndexMap<OwnedMxcUri, AvatarCacheEntry>> = RefCell::new(IndexMap::new());
}

/// An entry in the avatar cache.
#[derive(Clone)]
pub enum AvatarCacheEntry {
    Loaded(Arc<[u8]>),
    Requested,
    Failed,
}

pub struct AvatarUpdate {
    pub generation: u64,
    pub mxc_uri: OwnedMxcUri,
    pub avatar_data: Result<Arc<[u8]>, matrix_sdk::Error>,
}

/// The queue of avatar updates waiting to be processed by the UI thread's event handler.
static PENDING_AVATAR_UPDATES: std::sync::LazyLock<ArrayQueue<AvatarUpdate>> = std::sync::LazyLock::new(|| ArrayQueue::new(256));
static GENERATION: AtomicU64 = AtomicU64::new(0);
static OVERFLOW: AtomicBool = AtomicBool::new(false);
pub fn generation() -> u64 { GENERATION.load(Ordering::Acquire) }

/// Enqueues a new avatar update and signals the UI
/// such that the new update will be handled by the avatar sliding pane widget.
pub fn enqueue_avatar_update(mut update: AvatarUpdate) {
    if update.generation != generation() { return; }
    if let Ok(data) = &update.avatar_data {
        if !crate::memory_budget::admit_encoded(data) {
            // A lightweight failure still completes the reservation.
            update.avatar_data = Err(matrix_sdk::Error::UnknownError(anyhow::anyhow!("avatar memory budget exhausted").into()));
        }
    }
    if PENDING_AVATAR_UPDATES.push(update).is_err() { OVERFLOW.store(true, Ordering::Release); }
    SignalToUI::set_ui_signal();
}

/// Processes all pending avatar updates in the queue.
///
/// Triggers a redraw if any updates arrived.
pub fn process_avatar_updates(cx: &mut Cx) {
    if OVERFLOW.swap(false, Ordering::AcqRel) { clear_all_pending_and_failed_requests(); }
    let mut should_redraw = false;
    AVATAR_NEW_CACHE.with_borrow_mut(|cache| {
        let mut processed = 0;
        while processed < 64 {
            let Some(update) = PENDING_AVATAR_UPDATES.pop() else { break };
            processed += 1;
            if update.generation != generation() { continue; }
            // An old request may finish after account teardown; only accept a current reservation.
            if !matches!(cache.get(&update.mxc_uri), Some(AvatarCacheEntry::Requested)) { continue }
            cache.shift_remove(&update.mxc_uri);
            cache.insert(
                update.mxc_uri,
                match update.avatar_data {
                    Ok(data) => AvatarCacheEntry::Loaded(data),
                    Err(_e) => AvatarCacheEntry::Failed,
                },
            );
            should_redraw = true;
        }
        trim(cache);
    });
    if !PENDING_AVATAR_UPDATES.is_empty() { SignalToUI::set_ui_signal(); }
    if should_redraw {
        cx.redraw_all();
    }
}

/// Returns the cached avatar for the given Matrix URI if it exists,
/// or submits a request to fetch it from the server if it isn't already cached.
///
/// If a request has already been submitted, it will not re-submit a duplicate request
/// and will simply return `AvatarCacheEntry::Requested`.
///
/// This function requires passing in a reference to `Cx`,
/// which isn't used, but acts as a guarantee that this function
/// must only be called by the main UI thread.
pub fn get_or_fetch_avatar(
    _cx: &mut Cx,
    avatar_uri: &OwnedMxcUri,
) -> AvatarCacheEntry {
    AVATAR_NEW_CACHE.with_borrow_mut(|cache| {
        if let Some(entry) = cache.shift_remove(avatar_uri) {
            let result = entry.clone();
            cache.insert(avatar_uri.clone(), entry);
            return result;
        }
        trim(cache);
        // Bound new work without evicting a request that is already in flight.
        if cache.len() >= 256 { return AvatarCacheEntry::Requested }
        cache.insert(avatar_uri.clone(), AvatarCacheEntry::Requested);
        submit_async_request(MatrixRequest::FetchAvatar {
            mxc_uri: avatar_uri.clone(), on_fetched: enqueue_avatar_update,
        });
        AvatarCacheEntry::Requested
    })
}

/// Removes all `Requested` and `Failed` entries from the avatar cache,
/// allowing them to be re-fetched.
///
/// This should be called when the app transitions from offline back to online,
/// because any in-flight requests that were submitted while offline have likely
/// failed, leaving stale entries that permanently block re-fetching.
pub fn clear_all_pending_and_failed_requests() {
    AVATAR_NEW_CACHE.with_borrow_mut(|cache| {
        cache.retain(|_, entry| matches!(entry, AvatarCacheEntry::Loaded(_)));
    });
}

/// Clears cached avatars.
/// This function requires passing in a reference to `Cx`,
/// which acts as a guarantee that this function must only be called by the main UI thread.
pub fn clear_avatar_cache(_cx: &mut Cx) {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    while PENDING_AVATAR_UPDATES.pop().is_some() {}
    AVATAR_NEW_CACHE.with_borrow_mut(|cache| {
        cache.clear();
    });
}

fn trim(cache: &mut IndexMap<OwnedMxcUri, AvatarCacheEntry>) {
    let mut bytes = cache.values().map(|entry| match entry {
        AvatarCacheEntry::Loaded(data) => data.len(), _ => 0,
    }).sum::<usize>();
    let mut index = 0;
    while index < cache.len() && (bytes > 16 * 1024 * 1024 || cache.len() >= 256) {
        if matches!(cache.get_index(index).unwrap().1, AvatarCacheEntry::Requested) {
            index += 1;
        } else {
            let (_, entry) = cache.shift_remove_index(index).unwrap();
            if let AvatarCacheEntry::Loaded(data) = entry { bytes = bytes.saturating_sub(data.len()); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn avatar_budget_retains_inflight_reservations_and_recent_data() {
        let mut cache = IndexMap::new();
        for n in 0..260 {
            cache.insert(format!("mxc://example.org/{n}").into(), AvatarCacheEntry::Loaded(Arc::from([1u8])));
        }
        let pending: OwnedMxcUri = "mxc://example.org/pending".into();
        cache.insert(pending.clone(), AvatarCacheEntry::Requested);
        trim(&mut cache);
        assert!(cache.len() < 256);
        assert!(matches!(cache.get(&pending), Some(AvatarCacheEntry::Requested)));
        assert!(!cache.contains_key(&OwnedMxcUri::from("mxc://example.org/0")));
        assert!(cache.contains_key(&OwnedMxcUri::from("mxc://example.org/259")));
    }
}
