use std::ffi::OsString;
use std::path::Path;

use mcao_core::preflight::{
    CLASSIC_PATH_LIMIT, EXTENDED_PATH_LIMIT, PathCheck, PathLimit, TOOL_WARNING_LENGTH,
    check_home_outside_onedrive, check_path_len, projected_path_len,
};
use mcao_core::state::TaskId;

fn task(raw: &str) -> TaskId {
    TaskId::parse_for_creation(raw).expect("fixture task id is valid")
}

/// Spells a Unix-style absolute test path for the host. On Windows it gains a drive, because a
/// rooted path without one is not absolute there and the home check refuses it.
fn host(unix: &str) -> String {
    if cfg!(windows) {
        format!("C:{}", unix.replace('/', "\\"))
    } else {
        unix.to_owned()
    }
}

/// An environment lookup that knows exactly one variable.
fn only(key: &'static str, value: &str) -> impl Fn(&str) -> Option<OsString> {
    let value = OsString::from(value);
    move |name| (name == key).then(|| value.clone())
}

#[test]
fn long_paths_need_both_the_machine_and_git_settings() {
    assert_eq!(PathLimit::select(true, true), PathLimit::Extended);
    assert_eq!(PathLimit::select(true, false), PathLimit::Classic);
    assert_eq!(PathLimit::select(false, true), PathLimit::Classic);
    assert_eq!(PathLimit::select(false, false), PathLimit::Classic);
}

#[test]
fn limits_match_the_windows_ceilings() {
    assert_eq!(PathLimit::Classic.max_len(), 260);
    assert_eq!(PathLimit::Extended.max_len(), 32_767);
}

#[test]
fn projected_length_counts_both_separators_and_the_suffix() {
    // "C:/fm" (5) + "/" + "task" (4) + "/" + 10 units below the task directory.
    let len = projected_path_len(Path::new("C:/fm"), &task("task"), 10);

    assert_eq!(len, 5 + 1 + 4 + 1 + 10);
}

#[test]
fn projected_length_counts_a_supplementary_character_as_two_units() {
    // U+1F600 lies outside the Basic Multilingual Plane, so UTF-16 encodes it as two units.
    let len = projected_path_len(Path::new("/x\u{1F600}"), &task("a"), 0);

    assert_eq!(len, 4 + 1 + 1 + 1);
}

#[test]
fn a_path_at_the_tool_warning_length_is_within_budget() {
    let check = check_path_len(TOOL_WARNING_LENGTH, PathLimit::Classic).expect("within limit");

    assert_eq!(check, PathCheck::Within);
}

#[test]
fn a_path_past_the_tool_warning_length_passes_with_a_warning() {
    let check = check_path_len(TOOL_WARNING_LENGTH + 1, PathLimit::Classic).expect("within limit");

    assert_eq!(check, PathCheck::ToolsMayFail);
}

#[test]
fn a_path_one_unit_under_the_classic_limit_is_accepted_with_a_warning() {
    let check = check_path_len(CLASSIC_PATH_LIMIT - 1, PathLimit::Classic).expect("within limit");

    assert_eq!(check, PathCheck::ToolsMayFail);
}

#[test]
fn a_path_at_the_classic_limit_is_refused_because_the_nul_needs_a_unit() {
    let error = check_path_len(CLASSIC_PATH_LIMIT, PathLimit::Classic).expect_err("NUL is counted");
    let message = error.to_string();

    assert_eq!(error.exit_code(), 3);
    assert!(message.contains("260 UTF-16 units"), "{message}");
    assert!(message.contains("classic limit of 260"), "{message}");
    assert!(message.contains("LongPathsEnabled"), "{message}");
    assert!(message.contains("core.longpaths"), "{message}");
}

#[test]
fn the_extended_limit_admits_long_paths_and_refuses_at_it() {
    let check = check_path_len(EXTENDED_PATH_LIMIT - 1, PathLimit::Extended).expect("within limit");
    assert_eq!(check, PathCheck::ToolsMayFail);

    let error = check_path_len(EXTENDED_PATH_LIMIT, PathLimit::Extended).expect_err("at the limit");
    assert_eq!(error.exit_code(), 3);
    assert!(
        error.to_string().contains("extended limit of 32767"),
        "{error}"
    );
}

#[test]
fn a_home_outside_onedrive_passes() {
    check_home_outside_onedrive(
        Path::new(&host("/home/u/fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect("not under OneDrive");
}

#[test]
fn a_home_under_onedrive_is_refused_with_the_remedy() {
    let error = check_home_outside_onedrive(
        Path::new(&host("/home/u/OneDrive/fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect_err("under OneDrive");
    let message = error.to_string();

    assert_eq!(error.exit_code(), 3);
    assert!(message.contains("named by OneDrive"), "{message}");
    assert!(message.contains("set FM_HOME"), "{message}");
}

#[test]
fn a_business_onedrive_root_is_refused_too() {
    let error = check_home_outside_onedrive(
        Path::new(&host("/home/u/OneDriveCommercial/fm")),
        only("OneDriveCommercial", &host("/home/u/OneDriveCommercial")),
    )
    .expect_err("under the business OneDrive");

    assert!(
        error.to_string().contains("named by OneDriveCommercial"),
        "{error}"
    );
}

#[test]
fn a_sibling_folder_that_only_shares_a_name_prefix_passes() {
    check_home_outside_onedrive(
        Path::new(&host("/home/u/OneDriveBackup/fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect("OneDriveBackup is not under OneDrive");
}

#[test]
fn the_onedrive_root_matches_regardless_of_ascii_case() {
    let error = check_home_outside_onedrive(
        Path::new(&host("/home/u/onedrive/fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect_err("case-insensitive match");

    assert_eq!(error.exit_code(), 3);
}

#[test]
fn an_empty_onedrive_variable_names_no_root() {
    check_home_outside_onedrive(Path::new(&host("/home/u/fm")), only("OneDrive", ""))
        .expect("an empty root must not match every home");
}

#[test]
fn a_relative_home_is_refused_when_a_root_is_set() {
    let error =
        check_home_outside_onedrive(Path::new("fm"), only("OneDrive", &host("/home/u/OneDrive")))
            .expect_err("a relative home cannot be compared");
    let message = error.to_string();

    assert_eq!(error.exit_code(), 3);
    assert!(message.contains("cannot be checked"), "{message}");
    assert!(message.contains("named by OneDrive"), "{message}");
}

#[test]
fn a_relative_home_passes_when_no_root_is_set() {
    check_home_outside_onedrive(Path::new("fm"), |_| None)
        .expect("with no root there is nothing to compare");
}

#[test]
fn a_home_that_climbs_out_of_the_root_with_parent_components_is_refused() {
    // Lexically this is not under OneDrive, but `..` resolves it to `.../OneDrive/fm`. A textual
    // comparison would pass it, so the form is refused instead.
    let error = check_home_outside_onedrive(
        Path::new(&host("/home/u/Other/../OneDrive/fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect_err("`..` cannot be compared textually");

    assert_eq!(error.exit_code(), 3);
    assert!(error.to_string().contains("`..` components"), "{error}");
}

#[test]
fn a_home_under_the_root_with_parent_components_is_refused() {
    let error = check_home_outside_onedrive(
        Path::new(&host("/home/u/OneDrive/fm/../fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect_err("`..` cannot be compared textually");

    assert_eq!(error.exit_code(), 3);
}

#[cfg(windows)]
#[test]
fn a_verbatim_prefixed_home_is_refused_rather_than_passed() {
    let error = check_home_outside_onedrive(
        Path::new(r"\\?\C:\Users\u\OneDrive\fm"),
        only("OneDrive", r"C:\Users\u\OneDrive"),
    )
    .expect_err("a verbatim prefix differs from the root's disk prefix");

    assert_eq!(error.exit_code(), 3);
    assert!(error.to_string().contains("verbatim"), "{error}");
}

#[cfg(windows)]
#[test]
fn a_disk_prefixed_home_under_the_root_is_still_refused() {
    let error = check_home_outside_onedrive(
        Path::new(r"C:\Users\u\OneDrive\fm"),
        only("OneDrive", r"C:\Users\u\OneDrive"),
    )
    .expect_err("the plain disk form is compared and matches");

    assert!(error.to_string().contains("named by OneDrive"), "{error}");
}
