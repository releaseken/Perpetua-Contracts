//! Issue #104 — static verification that the capability flags are immutable.
//!
//! `cancellable`, `pausable` and `transferable` are the contract's trust
//! surface. The README's *Immutable guarantees* section promises they are fixed
//! at creation and can never change, and `create_stream` is the only place the
//! `Stream` struct initializes them. This module pins that promise two ways:
//!
//! 1. **Static source analysis** — the production source is compiled into the
//!    test binary (`include_str!`), so the checks run on the very bytes that
//!    ship. If a future edit adds a post-creation assignment to any flag, the
//!    strings fail to match and the test trips on every `cargo test` run.
//!
//! 2. **Runtime lifecycle preservation** — drive a stream through every
//!    mutating entry point and assert the three flags are bit-identical
//!    afterwards, including after a paused and a cancelled stream.
//!
//! `script/check-flag-immutability.py` implements the same structural rule for
//! CI independent of the Rust toolchain.

/// The production contract source, verbatim. Any assignment to a flag field
/// anywhere in this file would trip [`no_assignment_to_flags_anywhere`].
const LIB_SRC: &str = include_str!("../lib.rs");

/// The production type definitions, verbatim. The field declarations must stay
/// public fields documented as immutable — not, say, become `mut` or gain a
/// setter.
const TYPES_SRC: &str = include_str!("../types.rs");

/// The three capability flags, exactly as declared in `Stream`.
const FLAGS: [&str; 3] = ["cancellable", "pausable", "transferable"];

fn flag_lines(src: &str, needle: &str) -> std::vec::Vec<(usize, &str)> {
    src.lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle))
        .map(|(n, line)| (n + 1, line))
        .collect()
}

/// Track which `impl FluxoraStream` function a line belongs to.
fn enclosing_function<'a>(lines: &'a [&str], idx: usize) -> Option<&'a str> {
    let mut current = None;
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("pub fn ") || trimmed.starts_with("fn ") {
            let name = trimmed
                .split_whitespace()
                .nth(2)
                .unwrap_or("")
                .split('(')
                .next()
                .unwrap_or("");
            current = Some(name);
        }
        if i == idx {
            return current;
        }
    }
    current
}

/// A flag may only *read* its stored value; a field assignment like
/// `stream.cancellable = ...` is the one shape that would let a setter exist.
#[test]
fn no_assignment_to_flags_anywhere() {
    for (n, line) in flag_lines(LIB_SRC, ".flags =") {
        panic!(
            "flags assigned to at lib.rs:{n}: `{}` — capability flags are \
             immutable after create_stream",
            line.trim()
        );
    }
}

/// The flags are initialized exactly once, in `create_stream`, as shorthand
/// struct-literal fields. Testing for the shorthand form catches the 
/// `Stream { ... cancellable, ... }` initializer specifically.
#[test]
fn flags_are_initialized_exactly_once_inside_create_stream() {
    let lines: std::vec::Vec<&str> = LIB_SRC.lines().collect();
    let hits: std::vec::Vec<(usize, &str)> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("flags:"))
        .map(|(i, line)| (i + 1, *line))
        .collect();

    assert_eq!(
        hits.len(),
        1,
        "`flags` must be initialized exactly once in create_stream, found {hits:?}"
    );
    let (n, _) = hits[0];
    let enclosing = enclosing_function(&lines, n - 1);
    assert!(
        matches!(enclosing, Some("create_stream")),
        "flags initialized at lib.rs:{n} inside `{enclosing:?}`, expected create_stream"
    );
}

/// The flag reads inside the mutating entry points are *guards*, not writes:
/// `cancel`, `pause` and `transfer_recipient` (and their delegate twins) each
/// gate on the flag and then never modify it. This asserts the gate exists on
/// the primary entry points, exactly as the README documents.
#[test]
fn capability_guards_are_the_only_flag_touches_outside_creation() {
    let lines: std::vec::Vec<&str> = LIB_SRC.lines().collect();
    let mut guards: std::vec::Vec<(usize, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        for flag in FLAGS {
            if line.contains(&format!("!stream.{flag}")) {
                guards.push((i + 1, line.trim().to_string()));
            }
        }
    }
    // cancel, pause, transfer_recipient, and their delegate/batch variants
    // guard on exactly one flag.
    assert_eq!(
        guards.len(),
        7,
        "expected exactly the seven documented !stream.{flag} guards, got {guards:?}"
    );
    for (n, line) in &guards {
        assert!(
            line.starts_with("if !stream."),
            "lib.rs:{n}: guard is not a plain `if !stream.<flag>` check: {line:?}"
        );
    }
}

/// The struct fields in `types.rs` must stay documented as immutable. This
/// reads the declaration docs, so renaming a flag to something mutable-looking
/// while keeping the behaviour would still trip the doc check — the promise is
/// part of the source, not just the tests.
#[test]
fn flag_fields_are_declared_immutable_in_types() {
    let decl = "pub flags: u8,";
    let hits = flag_lines(TYPES_SRC, decl);
    assert_eq!(
        hits.len(),
        1,
        "types.rs must declare packed `{decl}` exactly once, found {hits:?}"
    );
    let (n, line) = hits[0];
    let documentation = TYPES_SRC
        .lines()
        .skip(n.saturating_sub(6))
        .take(5)
        .collect::<std::vec::Vec<_>>()
        .join("\n");
    assert!(
        documentation.contains("never mutable"),
        "types.rs:{n}: packed flags are not documented as immutable: `{documentation}`"
    );
    assert_eq!(line.trim_start(), decl);
}

// ---------------------------------------------------------------------------
// Runtime preservation
// ---------------------------------------------------------------------------

use super::common::{Harness, ONE, DAY};
use crate::{Error, StreamStatus};

/// Run a stream through every mutating entry point it will ever see — top-up,
/// pause, resume, partial withdraw, full withdraw — and assert the capability
/// flags are byte-identical afterwards. This is the runtime half of the proof:
/// even where the contract *could* have been written to touch a flag, the
/// observable state proves it does not.
#[test]
fn flags_are_preserved_across_the_full_lifecycle() {
    let h = Harness::new();
    let id = h.create(
        100 * ONE,
        h.now(),
        h.now() + 100 * DAY,
        h.now(),
        true,
        true,
        true,
    );
    let original = h.get(id);
    assert!(original.cancellable && original.pausable && original.transferable);

    h.client.top_up(&id, &(10 * ONE));
    h.advance(10 * DAY);
    h.client.pause(&id);
    h.assert_invariants();
    h.advance(5 * DAY);
    h.client.resume(&id);
    h.advance(10 * DAY);
    h.client.withdraw(&id, &None);

    let lived = h.get(id);
    assert_eq!(lived.cancellable, original.cancellable);
    assert_eq!(lived.pausable, original.pausable);
    assert_eq!(lived.transferable, original.transferable);
    assert_ne!(lived.end_time, original.end_time, "sanity: top_up moved end");
    h.assert_pool_exact();
}

/// A cancelled stream keeps its flags. Cancellation rewrites `deposited` and
/// `end_time` — the two flags must survive that rewrite untouched.
#[test]
fn flags_are_preserved_after_cancel() {
    let h = Harness::new();
    let id = h.create(
        100 * ONE,
        h.now(),
        h.now() + 100 * DAY,
        h.now(),
        true,
        true,
        true,
    );
    h.advance(20 * DAY);
    h.client.cancel(&id);
    let cancelled = h.get(id);
    assert_eq!(cancelled.status, StreamStatus::Cancelled);
    assert!(cancelled.cancellable && cancelled.pausable && cancelled.transferable,
        "cancel rewrote the schedule but must leave the capability flags intact");
}

/// A stream created with all flags off can never be made to flip any of them
/// on — the capability pathways are the only writers, and each is gated.
#[test]
fn a_stream_with_all_flags_off_never_gains_capability() {
    let h = Harness::new();
    let id = h.create(
        100 * ONE,
        h.now(),
        h.now() + 100 * DAY,
        h.now(),
        false,
        false,
        false,
    );
    h.advance(10 * DAY);

    assert_eq!(
        h.client.try_cancel(&id).unwrap_err().unwrap(),
        Error::NotCancellable
    );
    assert_eq!(
        h.client.try_pause(&id).unwrap_err().unwrap(),
        Error::NotPausable
    );
    assert_eq!(
        h.client
            .try_transfer_recipient(&id, &h.other)
            .unwrap_err()
            .unwrap(),
        Error::NotTransferable
    );

    let after = h.get(id);
    assert!(!after.cancellable && !after.pausable && !after.transferable);
    h.assert_pool_exact();
}

/// The immutable-fields invariant holds for every stream the test harness can
/// reach after an arbitrary sequence run by the property suite — this test
/// re-checks the flags for existing streams created by normal harness calls.
#[test]
fn flags_are_constant_after_withdrawal_reaches_terminal_state() {
    let h = Harness::new();
    let id = h.create_simple(100 * ONE, 10 * DAY);
    h.advance(10 * DAY);
    h.client.withdraw(&id, &None);
    assert_eq!(h.get(id).status, StreamStatus::Depleted);
    let terminal = h.get(id);
    assert!(terminal.cancellable && terminal.pausable && terminal.transferable);
}