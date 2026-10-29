use mcao_core::{EvidenceRecord, Execution, Operation, Outcome, Support};
use mcao_probe::args::Scenario;
use mcao_probe::lifecycle::{STOP_MARKER, run_lifecycle};
use mcao_probe::mock::{HARNESS, MockBackend};

const ORDER: [Operation; 7] = [
    Operation::Spawn,
    Operation::Capture,
    Operation::SendText,
    Operation::Interrupt,
    Operation::AgentState,
    Operation::Reconnect,
    Operation::Cleanup,
];

fn run(scenario: Scenario) -> Vec<EvidenceRecord> {
    let mut backend = MockBackend::new(scenario);
    run_lifecycle(&mut backend, HARNESS)
}

fn find(records: &[EvidenceRecord], operation: Operation) -> &EvidenceRecord {
    records
        .iter()
        .find(|record| record.observation().operation == operation)
        .expect("operation recorded")
}

#[test]
fn cooperative_run_records_all_seven_operations_in_order() {
    let records = run(Scenario::Cooperative);
    let operations: Vec<Operation> = records
        .iter()
        .map(|record| record.observation().operation)
        .collect();
    assert_eq!(operations, ORDER);
}

#[test]
fn cooperative_run_is_mock_only_and_never_demonstrated() {
    for record in run(Scenario::Cooperative) {
        assert_eq!(record.observation().execution, Execution::Mock);
        assert_eq!(record.observation().outcome, Outcome::Success);
        assert_eq!(record.support(), Support::MockOnly);
    }
}

#[test]
fn cooperative_interrupt_is_judged_by_stop_marker_and_idle_state() {
    let records = run(Scenario::Cooperative);
    let interrupt = find(&records, Operation::Interrupt);
    let state = find(&records, Operation::AgentState);
    assert_eq!(interrupt.observation().outcome, Outcome::Success);
    assert_eq!(state.observation().output_sample.as_deref(), Some("idle"));
}

#[test]
fn cooperative_mock_prints_the_probe_stop_marker_after_an_escape() {
    use mcao_core::SessionBackend;

    let mut backend = MockBackend::new(Scenario::Cooperative);
    let handle = backend.spawn(HARNESS).expect("mock spawn");
    backend.send_text(&handle, "work").expect("mock send_text");
    backend.interrupt(&handle).expect("mock interrupt");
    let output = backend.capture(&handle).expect("mock capture");
    assert!(
        output.contains(STOP_MARKER),
        "mock output lacks the probe stop marker {STOP_MARKER:?}: {output:?}"
    );
}

#[test]
fn uncooperative_interrupt_is_unresolved_with_reason_and_run_continues() {
    let records = run(Scenario::Uncooperative);

    let interrupt = find(&records, Operation::Interrupt);
    assert_eq!(interrupt.support(), Support::Unresolved);
    match &interrupt.observation().outcome {
        Outcome::Failed { reason } => {
            assert_eq!(reason, "agent still running after one Escape");
        }
        other => panic!("expected failed interrupt, got {other:?}"),
    }

    let state = find(&records, Operation::AgentState);
    assert_eq!(
        state.observation().output_sample.as_deref(),
        Some("running")
    );

    // A failed interrupt must not stop the run: later operations still report.
    assert_eq!(
        find(&records, Operation::Reconnect).support(),
        Support::MockOnly
    );
    assert_eq!(
        find(&records, Operation::Cleanup).support(),
        Support::MockOnly
    );
}

#[test]
fn capture_sample_shows_the_mock_session_output() {
    let records = run(Scenario::Cooperative);
    let capture = find(&records, Operation::Capture);
    assert_eq!(
        capture.observation().output_sample.as_deref(),
        Some("mock harness ready\n")
    );
}

#[test]
fn every_record_carries_the_same_handle_after_spawn() {
    let records = run(Scenario::Cooperative);
    let spawn_handle = find(&records, Operation::Spawn)
        .observation()
        .handle
        .clone();
    assert_eq!(spawn_handle.as_deref(), Some("mock-1"));
    for record in records.iter().skip(1) {
        assert_eq!(record.observation().handle, spawn_handle);
    }
}
