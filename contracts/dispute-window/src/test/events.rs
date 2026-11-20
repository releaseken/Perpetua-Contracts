//! Verifies that every event topic name is unique (mirrors the stream contract's
//! `test::events::test_all_event_topic_names_are_unique`).

#![cfg(test)]

use soroban_sdk::Env;

use crate::events::EVENT_TOPIC_NAMES;

/// Every `topic[0]` symbol must be unique across all event structs.
///
/// A collision would make two distinct event kinds indistinguishable to an
/// indexer that filters by topic.
#[test]
fn test_all_event_topic_names_are_unique() {
    let mut seen: std::vec::Vec<&str> = std::vec::Vec::new();
    for name in EVENT_TOPIC_NAMES.iter() {
        assert!(
            !seen.contains(name),
            "duplicate event topic name: {name}",
        );
        seen.push(name);
    }
    assert_eq!(
        seen.len(),
        EVENT_TOPIC_NAMES.len(),
        "topic name count mismatch",
    );
}

/// Verify the inventory length matches the number of event structs defined in
/// `events.rs`.  Update this count when adding new events.
#[test]
fn test_event_topic_inventory_length() {
    assert_eq!(
        EVENT_TOPIC_NAMES.len(),
        7,
        "EVENT_TOPIC_NAMES has unexpected length; update this test when adding events",
    );
}
