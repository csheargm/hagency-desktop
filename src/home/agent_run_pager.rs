//! Which run each of a message's fixed run-card slots shows. Every run stays
//! reachable: all slots but the last show one run each, and the last pages
//! through the rest, so an answer or an active run's controls never drop off.
use crate::hagency::run_controls::Entry;

/// Run-card slots drawn under one message (the DSL holds this many).
pub const SLOTS: usize = 3;

/// The runs each slot shows, in order; the last slot holds the overflow.
pub fn slot_entries(entries: &[Entry]) -> [&[Entry]; SLOTS] {
    let one = |i: usize| entries.get(i..(i + 1).min(entries.len())).unwrap_or_default();
    [one(0), one(1), entries.get(SLOTS - 1..).unwrap_or_default()]
}

/// The page to show: the run the reader paged to while it is still there, else the first.
pub fn page_of(keys: &[&str], selected: Option<&str>) -> usize {
    selected.and_then(|key| keys.iter().position(|k| *k == key)).unwrap_or(0)
}

/// The page after a step, wrapping at both ends.
pub fn step(page: usize, total: usize, forward: bool) -> usize {
    if total == 0 { return 0; }
    if forward { (page + 1) % total } else { (page + total - 1) % total }
}

/// "2 / 5" under a slot that pages; empty when there is nothing to page.
pub fn pager_text(page: usize, total: usize) -> String {
    if total > 1 { format!("{} / {total}", page + 1) } else { String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_slot_pages_through_every_run_beyond_the_first_ones() {
        assert_eq!(page_of(&["a", "b", "c"], Some("c")), 2);
        assert_eq!(page_of(&["a", "b"], Some("gone")), 0, "a run no longer there falls back to the first");
        assert_eq!(step(0, 3, false), 2);
        assert_eq!(step(2, 3, true), 0);
        assert_eq!(pager_text(1, 4), "2 / 4");
        assert_eq!(pager_text(0, 1), "", "one run needs no pager");
    }
}
