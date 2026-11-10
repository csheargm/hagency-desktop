//! Account-local emoji frequency and skin-tone preferences. Adapted from Robrix
//! 0f299879 without changing Hagency's Matrix SDK or importing legacy account data.

use std::collections::HashMap;
use emojis::{Emoji, SkinTone};
use makepad_widgets::Cx;
use ruma::OwnedUserId;
use serde::{Deserialize, Serialize};
use super::emoji_catalog::{can_draw, without_skin_tone};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecentEmojis {
    picked: Vec<(String, u64)>,
    skin_tone: Option<SkinTone>,
}

#[derive(Default)]
struct RecentEmojisGlobal {
    user_id: Option<OwnedUserId>,
    saved: RecentEmojis,
}

fn current(cx: &mut Cx) -> &mut RecentEmojisGlobal {
    let user_id = crate::sliding_sync::current_user_id();
    let global = cx.global::<RecentEmojisGlobal>();
    if global.user_id != user_id {
        *global = RecentEmojisGlobal {
            user_id,
            ..Default::default()
        };
    }
    global
}

impl RecentEmojis {
    fn record(&mut self, emoji: &str) {
        let Some(emoji) = emojis::get(emoji) else {
            return;
        };
        let key = emoji.as_str();
        let count = self
            .picked
            .iter()
            .position(|(value, _)| value == key)
            .map(|index| self.picked.remove(index).1)
            .unwrap_or(0);
        self.picked
            .insert(0, (key.to_owned(), count.saturating_add(1)));
        self.picked.truncate(256);
    }
}

pub fn record_emoji_pick(cx: &mut Cx, emoji: &str) {
    let global = current(cx);
    if global.user_id.is_some() {
        global.saved.record(emoji);
    }
}

pub fn get_frequently_used_emojis(cx: &mut Cx, max: usize) -> Vec<&'static Emoji> {
    let mut recent: Vec<_> = current(cx)
        .saved
        .picked
        .iter()
        .filter_map(|(text, count)| Some((emojis::get(text).filter(|e| can_draw(e))?, *count)))
        .take(max)
        .collect();
    recent.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    recent.into_iter().map(|(emoji, _)| emoji).collect()
}

pub fn get_pick_counts(cx: &mut Cx) -> HashMap<&'static str, u64> {
    let mut counts: HashMap<&'static str, u64> = HashMap::new();
    for (value, count) in &current(cx).saved.picked {
        if let Some(emoji) = emojis::get(value) {
            let value = counts.entry(without_skin_tone(emoji).as_str()).or_default();
            *value = value.saturating_add(*count);
        }
    }
    counts
}

pub fn get_skin_tone(cx: &mut Cx) -> SkinTone {
    current(cx).saved.skin_tone.unwrap_or(SkinTone::Default)
}
pub fn set_skin_tone(cx: &mut Cx, tone: SkinTone) {
    current(cx).saved.skin_tone = Some(tone);
}

pub fn save_recent_emojis(cx: &mut Cx, user_id: &OwnedUserId) -> RecentEmojis {
    let global = current(cx);
    if global.user_id.as_ref() == Some(user_id) {
        global.saved.clone()
    } else {
        RecentEmojis::default()
    }
}

pub fn restore_recent_emojis(cx: &mut Cx, user_id: &OwnedUserId, saved: RecentEmojis) {
    if crate::sliding_sync::current_user_id().as_ref() == Some(user_id) {
        *cx.global::<RecentEmojisGlobal>() = RecentEmojisGlobal {
            user_id: Some(user_id.clone()),
            saved,
        };
    }
}

pub fn clear_recent_emojis(cx: &mut Cx) {
    *cx.global::<RecentEmojisGlobal>() = RecentEmojisGlobal::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picks_are_canonical_counted_and_roundtrip_with_skin_tone() {
        let mut recent = RecentEmojis::default();
        recent.record("❤");
        recent.record("❤️");
        recent.record("not an emoji");
        assert_eq!(recent.picked, vec![("❤️".to_owned(), 2)]);
        recent.skin_tone = Some(SkinTone::Dark);
        let restored: RecentEmojis =
            serde_json::from_slice(&serde_json::to_vec(&recent).unwrap()).unwrap();
        assert_eq!(restored, recent);
    }
}
