//! Window lifecycle tests: init, open, close, finalise.

#![cfg(test)]

use soroban_sdk::testutils::Address as _;
use soroban_sdk::{Address, Env};

use crate::{DisputeError, DisputeWindow, DisputeWindowClient, WindowStatus};

use super::common::*;

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

#[test]
fn test_init_sets_config() {
    let h = Harness::new();
    let cfg = h.client.get_config();
    assert_eq!(cfg.admin, h.admin);
    assert_eq!(cfg.arbiter, h.arbiter);
    assert_eq!(cfg.stream_contract, h.stream_contract);
    assert_eq!(cfg.status, WindowStatus::Pending);
}

#[test]
fn test_init_twice_errors() {
    let h = Harness::new();
    let result = h
        .client
        .try_init(&h.admin, &h.stream_contract, &h.arbiter);
    assert_eq!(result, Err(Ok(DisputeError::AlreadyInitialized)));
}

// ---------------------------------------------------------------------------
// open_window
// ---------------------------------------------------------------------------

#[test]
fn test_open_window_transitions_to_open() {
    let h = Harness::new();
    h.client.open_window(&(T0 + WEEK));
    assert_eq!(h.client.get_config().status, WindowStatus::Open);
}

#[test]
fn test_open_window_records_times() {
    let h = Harness::new();
    let deadline = T0 + WEEK;
    h.client.open_window(&deadline);
    let cfg = h.client.get_config();
    assert_eq!(cfg.open_time, T0);
    assert_eq!(cfg.deadline, deadline);
}

#[test]
fn test_open_window_in_past_errors() {
    let h = Harness::new();
    let past_deadline = T0 - 1;
    let result = h.client.try_open_window(&past_deadline);
    assert_eq!(result, Err(Ok(DisputeError::InvalidClaimAmount)));
}

#[test]
fn test_open_window_twice_errors() {
    let h = Harness::new_open();
    let result = h.client.try_open_window(&(T0 + 2 * WEEK));
    assert_eq!(result, Err(Ok(DisputeError::WindowAlreadyOpen)));
}

// ---------------------------------------------------------------------------
// close_window
// ---------------------------------------------------------------------------

#[test]
fn test_close_window_transitions_to_closed() {
    let h = Harness::new_open();
    h.client.close_window();
    assert_eq!(h.client.get_config().status, WindowStatus::Closed);
}

#[test]
fn test_close_window_on_pending_errors() {
    let h = Harness::new();
    let result = h.client.try_close_window();
    assert_eq!(result, Err(Ok(DisputeError::WindowNotOpen)));
}

// ---------------------------------------------------------------------------
// finalise_window
// ---------------------------------------------------------------------------

#[test]
fn test_finalise_window_from_open() {
    let h = Harness::new_open();
    h.client.finalise_window();
    assert_eq!(h.client.get_config().status, WindowStatus::Finalised);
}

#[test]
fn test_finalise_window_from_closed() {
    let h = Harness::new_open();
    h.client.close_window();
    h.client.finalise_window();
    assert_eq!(h.client.get_config().status, WindowStatus::Finalised);
}

#[test]
fn test_finalise_window_pending_errors() {
    let h = Harness::new();
    let result = h.client.try_finalise_window();
    assert_eq!(result, Err(Ok(DisputeError::WindowNotOpen)));
}

#[test]
fn test_finalise_window_twice_errors() {
    let h = Harness::new_open();
    h.client.finalise_window();
    let result = h.client.try_finalise_window();
    assert_eq!(result, Err(Ok(DisputeError::WindowNotOpen)));
}

// ---------------------------------------------------------------------------
// Admin rotation
// ---------------------------------------------------------------------------

#[test]
fn test_set_admin_rotates() {
    let h = Harness::new();
    let new_admin = Address::generate(&h.env);
    h.client.set_admin(&new_admin);
    assert_eq!(h.client.get_config().admin, new_admin);
}

#[test]
fn test_set_arbiter_rotates() {
    let h = Harness::new();
    let new_arbiter = Address::generate(&h.env);
    h.client.set_arbiter(&new_arbiter);
    assert_eq!(h.client.get_config().arbiter, new_arbiter);
}

// ---------------------------------------------------------------------------
// Before init
// ---------------------------------------------------------------------------

#[test]
fn test_operations_before_init_error() {
    let env = Env::default();
    env.mock_all_auths();
    let cid = env.register(DisputeWindow, ());
    let client = DisputeWindowClient::new(&env, &cid);

    // open_window before init must return NotInitialized
    let result = client.try_open_window(&(T0 + WEEK));
    assert_eq!(result, Err(Ok(DisputeError::NotInitialized)));
}
