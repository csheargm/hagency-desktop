use super::*;
use crate::hagency::pending_questions as pq;
use serde_json::json;

/// A loaded card with event id `id` and no edit.
fn card(id: &str) -> Loaded {
    let event = json!({"event_id": id, "content": {"msgtype": ::hagency::agent_run_controls::MSGTYPE}});
    super::super::cards_of(event, None, None).unwrap()
}

/// GPT review P2 (round 2): over `MAX_HANDED` the table forgot every settled
/// claim and failure count, so cards out of tries were fetched again. A
/// timeline with more keys than that keeps them out, and the size stays bounded.
#[test]
fn over_the_cap_out_of_tries_cards_are_not_claimed_again_and_the_table_stays_bounded() {
    let _guard = pq::test_serial();
    let generation = u64::MAX - 7;
    let exhausted = card("$exhausted");
    {
        let mut handed = HANDED.lock().unwrap_or_else(|e| e.into_inner());
        handed.clear();
        handed.insert(exhausted.key(generation), Handed::Failed(MAX_TRIES));
        for n in 0..MAX_HANDED + 10 {
            handed.insert((generation, format!("$stored{n}"), String::new()), Handed::Stored);
        }
    }
    let claimed = claim(generation, vec![card("$exhausted"), card("$fresh")]);
    assert_eq!(claimed.iter().map(|c| c.card.as_str()).collect::<Vec<_>>(), ["$fresh"], "the exhausted card is not fetched again");
    let len = HANDED.lock().unwrap_or_else(|e| e.into_inner()).len();
    assert!(len <= MAX_HANDED, "bounded: {len}");
    assert!(claim(generation, vec![card("$exhausted")]).is_empty(), "still out of tries on the next ingest");

    // Even a table of only out-of-tries cards stays bounded.
    {
        let mut handed = HANDED.lock().unwrap_or_else(|e| e.into_inner());
        for n in 0..2 * MAX_HANDED {
            handed.insert((generation, format!("$failed{n}"), String::new()), Handed::Failed(MAX_TRIES));
        }
    }
    claim(generation, vec![]);
    let len = HANDED.lock().unwrap_or_else(|e| e.into_inner()).len();
    assert!(len <= MAX_HANDED, "bounded: {len}");
    HANDED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// GPT re-review P2: the rooms-list path claimed without pruning, and a large
/// batch could leave the table past the cap. Both stay bounded once settled.
#[test]
fn rooms_list_claims_and_large_batches_keep_the_table_bounded() {
    let _guard = pq::test_serial();
    let generation = u64::MAX - 8;
    HANDED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let batch: Vec<_> = (0..MAX_HANDED + 50).map(|n| card(&format!("$batch{n}"))).collect();
    let claimed = claim(generation, batch);
    for c in &claimed {
        settle_key(c.key(generation), true);
    }
    claim(generation, vec![]);
    for n in 0..MAX_HANDED + 50 {
        if claim_one((generation, format!("$latest{n}"), String::new())) {
            settle_key((generation, format!("$latest{n}"), String::new()), true);
        }
    }
    let len = HANDED.lock().unwrap_or_else(|e| e.into_inner()).len();
    assert!(len <= MAX_HANDED + 1, "bounded: {len}");
    HANDED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
