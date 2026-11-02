//! Parity tests for `mcao util aggregate-timing-json` and `mcao util write-timing-json`. The
//! expected documents in `tests/fixtures/util-timing` were produced once by the python3 heredocs
//! that `bin/fm-test-run.sh` used before this port, run from that directory with the same inputs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/util-timing")
}

fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn mcao_in_fixtures(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_mcao"))
        .current_dir(fixtures())
        .args(args)
        .output()
        .expect("run mcao")
}

#[test]
fn aggregate_matches_the_python_reference_bytes() {
    let out = scratch("timing-aggregate").join("aggregate.json");
    let output = mcao_in_fixtures(&[
        "util",
        "aggregate-timing-json",
        out.to_str().expect("utf-8 path"),
        "a.json",
        "b.json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "FM_TEST_AGGREGATE lanes=2 total=5 failed=1 skipped_gate=1 critical_path_duration_ms=2000\n"
    );
    let expected = fs::read(fixtures().join("expected-aggregate.json")).expect("fixture");
    assert_eq!(fs::read(&out).expect("aggregate output"), expected);
}

#[test]
fn aggregate_normalizes_lane_paths_like_pathlib() {
    let out = scratch("timing-aggregate-paths").join("aggregate.json");
    let output = mcao_in_fixtures(&[
        "util",
        "aggregate-timing-json",
        out.to_str().expect("utf-8 path"),
        "./a.json",
        ".//b.json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = fs::read(fixtures().join("expected-aggregate.json")).expect("fixture");
    assert_eq!(fs::read(&out).expect("aggregate output"), expected);
}

fn aggregate_lane_paths(name: &str, inputs: &[&str]) -> Vec<String> {
    let out = scratch(name).join("aggregate.json");
    let mut args = vec![
        "util",
        "aggregate-timing-json",
        out.to_str().expect("utf-8 path"),
    ];
    args.extend_from_slice(inputs);
    let output = mcao_in_fixtures(&args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&fs::read(&out).expect("aggregate output")).expect("aggregate JSON");
    doc["lanes"]
        .as_array()
        .expect("lanes array")
        .iter()
        .map(|lane| lane["path"].as_str().expect("lane path").to_owned())
        .collect()
}

#[cfg(unix)]
#[test]
fn lane_paths_keep_a_leading_double_slash_and_collapse_triple_slashes() {
    let file = fixtures().join("a.json");
    let absolute = file.to_str().expect("utf-8 path").trim_start_matches('/');
    let double = format!("//{absolute}");
    let triple = format!("///{absolute}");
    let paths = aggregate_lane_paths(
        "timing-lane-paths-absolute",
        &[double.as_str(), triple.as_str()],
    );
    assert_eq!(paths, [double.clone(), format!("/{absolute}")]);
}

#[test]
fn lane_paths_keep_dotdot_and_collapse_redundant_separators() {
    let paths = aggregate_lane_paths(
        "timing-lane-paths-dotdot",
        &["../util-timing/a.json", "./..//util-timing///./b.json"],
    );
    assert_eq!(paths, ["../util-timing/a.json", "../util-timing/b.json"]);
}

#[test]
fn lane_paths_drop_trailing_slashes_before_reading() {
    let paths = aggregate_lane_paths(
        "timing-lane-paths-trailing",
        &["a.json/", "./../util-timing//b.json/./"],
    );
    assert_eq!(paths, ["a.json", "../util-timing/b.json"]);
}

#[test]
fn write_matches_the_python_reference_bytes() {
    let out = scratch("timing-write").join("timing.json");
    let output = mcao_in_fixtures(&[
        "util",
        "write-timing-json",
        out.to_str().expect("utf-8 path"),
        "2026-07-22T00:00:00Z",
        "2026-07-22T00:01:00Z",
        "run-ü",
        "5",
        "1",
        "2",
        "1234",
        "selection ü",
        "records.tsv",
        "families.tsv",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = fs::read(fixtures().join("expected-write.json")).expect("fixture");
    assert_eq!(fs::read(&out).expect("timing output"), expected);
}

#[test]
fn timing_subcommands_refuse_missing_arguments_with_usage() {
    let aggregate = mcao_in_fixtures(&["util", "aggregate-timing-json", "out.json"]);
    assert_eq!(aggregate.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&aggregate.stderr)
            .starts_with("usage: mcao util aggregate-timing-json")
    );

    let write = mcao_in_fixtures(&["util", "write-timing-json", "out.json"]);
    assert_eq!(write.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&write.stderr).starts_with("usage: mcao util write-timing-json")
    );
}

#[test]
fn write_rejects_a_record_line_with_the_wrong_field_count() {
    let dir = scratch("timing-bad-records");
    let records = dir.join("records.tsv");
    fs::write(&records, "tests/a.test.sh\tafk\tfalse\t0\n").expect("write records");
    let out = dir.join("timing.json");
    let output = Command::new(env!("CARGO_BIN_EXE_mcao"))
        .args([
            "util",
            "write-timing-json",
            out.to_str().expect("utf-8 path"),
            "s",
            "f",
            "r",
            "0",
            "0",
            "0",
            "0",
            "sel",
            records.to_str().expect("utf-8 path"),
            fixtures()
                .join("families.tsv")
                .to_str()
                .expect("utf-8 path"),
        ])
        .output()
        .expect("run mcao");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("expected 7 tab-separated fields, found 4")
    );
    assert!(
        !out.exists(),
        "a rejected record file must not produce an artifact"
    );
}
