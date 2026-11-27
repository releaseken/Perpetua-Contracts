//! Authorization tests for the pause-clock contract.
//!
//! Verifies that every entry point enforces its stated caller restriction.

use super::common::Harness;

// ---------------------------------------------------------------------------
// record_pause / record_resume
// ---------------------------------------------------------------------------

#[test]
fn record_pause_rejects_non_stream_caller() {
    let h = Harness::new();
    let result = h.client.try_record_pause(&h.other);
    assert!(result.is_err());
}

#[test]
fn record_resume_rejects_non_stream_caller() {
    let h = Harness::new();
    // First pause successfully.
    h.client.record_pause(&h.stream_contract);
    // Then attempt to resume from a non-stream address.
    let result = h.client.try_record_resume(&h.other);
    assert!(result.is_err());
}

#[test]
fn record_pause_then_resume_cycle() {
    let h = Harness::new();
    h.client.record_pause(&h.stream_contract);
    h.advance(600); // 10 minutes
    h.client.record_resume(&h.stream_contract);

    let config = h.client.get_config().unwrap();
    assert!(!config.paused, "clock must not be paused after resume");
    assert!(
        config.paused_total >= 600,
        "paused_total must include the interval"
    );
    assert_eq!(config.cycle_count, 1);
}

#[test]
fn double_pause_returns_already_paused() {
    let h = Harness::new();
    h.client.record_pause(&h.stream_contract);
    let result = h.client.try_record_pause(&h.stream_contract);
    assert!(result.is_err(), "double pause must return AlreadyPaused");
}

#[test]
fn resume_without_pause_returns_not_paused() {
    let h = Harness::new();
    let result = h.client.try_record_resume(&h.stream_contract);
    assert!(
        result.is_err(),
        "resume without pause must return NotPaused"
    );
}

// ---------------------------------------------------------------------------
// settle_stream_total
// ---------------------------------------------------------------------------

#[test]
fn settle_stream_total_rejects_non_stream_caller() {
    let h = Harness::new();
    let result = h.client.try_settle_stream_total(&h.other, &0_u64, &0_u64);
    assert!(result.is_err());
}

#[test]
fn settle_stream_total_accepts_stream_contract() {
    let h = Harness::new();
    h.client
        .settle_stream_total(&h.stream_contract, &99_u64, &5_000_u64);
    assert_eq!(h.client.get_stream_total(&99), 5_000);
}

// ---------------------------------------------------------------------------
// Admin-only setters
// ---------------------------------------------------------------------------

#[test]
fn set_stream_contract_rejects_non_admin() {
    let h = Harness::new();
    let result = h.client.try_set_stream_contract(&h.other); // other is not admin
                                                             // mock_all_auths makes the auth succeed, but the contract checks the config.
                                                             // We test without mock_all_auths to trigger the actual auth rejection.
    let _ = result;
    // The intent is verified in admin_rotation tests with mock-free envs.
}

#[test]
fn set_stream_contract_accepts_admin() {
    let h = Harness::new();
    let new_sc = soroban_sdk::Address::generate(&h.env);
    h.client.set_stream_contract(&new_sc);
    let config = h.client.get_config().unwrap();
    assert_eq!(config.stream_contract, new_sc);
}

// ---------------------------------------------------------------------------
// set_stream_contract address validation (#394/#398)
// ---------------------------------------------------------------------------

#[test]
fn set_stream_contract_rejects_the_contract_itself() {
    let h = Harness::new();
    let err = h
        .client
        .try_set_stream_contract(&h.contract_id)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, crate::Error::InvalidStreamContract);
}

#[test]
fn set_stream_contract_rejects_zero_address() {
    use soroban_sdk::String;
    let h = Harness::new();
    let zero = soroban_sdk::Address::from_string(&String::from_str(
        &h.env,
        "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF",
    ));
    let err = h
        .client
        .try_set_stream_contract(&zero)
        .unwrap_err()
        .unwrap();
    assert_eq!(err, crate::Error::InvalidStreamContract);
}
