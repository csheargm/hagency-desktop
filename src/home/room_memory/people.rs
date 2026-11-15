//! Who wrote and who accepted each memory entry, as names people read: the
//! member's display name, "Agent" for an unnamed Agent, never a raw Matrix id.
use std::collections::BTreeMap;

/// Display names by Matrix id, and the accepting user by entry id.
#[derive(Clone, Debug, Default)]
pub struct People {
    pub names: BTreeMap<String, String>,
    pub accepted_by: BTreeMap<String, String>,
}
impl People {
    /// The name shown for `user`.
    pub fn name(&self, user: &str) -> String {
        let localpart = user.trim_start_matches('@').split(':').next().unwrap_or(user);
        super::intro_name(self.names.get(user).cloned(), localpart)
    }
}

/// The sender of the latest revision of each entry (state storage: whoever wrote the state).
pub fn acceptors(events: &[serde_json::Value]) -> BTreeMap<String, String> {
    let mut latest: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for event in events {
        let (Some(id), Some(sender)) = (event["state_key"].as_str(), event["sender"].as_str()) else { continue };
        let rev = event["content"]["rev"].as_u64().unwrap_or(0);
        if latest.get(id).is_none_or(|(r, _)| rev >= *r) {
            latest.insert(id.to_owned(), (rev, sender.to_owned()));
        }
    }
    latest.into_iter().map(|(id, (_, sender))| (id, sender)).collect()
}

/// "Pinned · Suggested by demoup-claude · accepted by demoup" (or "Added by …").
pub fn meta_line(entry: &hagency::room_memory::Entry, people: &People) -> String {
    let mut parts = Vec::new();
    if entry.pinned {
        parts.push(crate::i18n::tr("Pinned").to_owned());
    }
    if !entry.author.is_empty() {
        let author = people.name(&entry.author);
        match people.accepted_by.get(&entry.id).filter(|by| **by != entry.author) {
            Some(by) => parts.push(crate::i18n::format("Suggested by {author} · accepted by {owner}", &[("author", author), ("owner", people.name(by))])),
            None => parts.push(crate::i18n::format("Added by {author}", &[("author", author)])),
        }
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn entries_name_people_never_raw_ids() {
        let mut people = People::default();
        people.names.insert("@_hagency_agt_01m7:x".into(), "demoup-claude".into());
        people.accepted_by = acceptors(&[json!({"state_key":"m1","sender":"@demoup:x","content":{"rev":1}})]);
        let entry = hagency::room_memory::Entry { id: "m1".into(), text: "t".into(), author: "@_hagency_agt_01m7:x".into(), source_event: None, pinned: false, rev: 1 };
        assert_eq!(meta_line(&entry, &people), crate::i18n::format("Suggested by {author} · accepted by {owner}", &[("author", "demoup-claude".into()), ("owner", "demoup".into())]));
        let unnamed = hagency::room_memory::Entry { author: "@_hagency_agt_99:x".into(), id: "m2".into(), ..entry };
        assert!(meta_line(&unnamed, &people).contains(crate::i18n::tr("Agent")));
        assert!(!meta_line(&unnamed, &people).contains("agt_"));
    }
}
