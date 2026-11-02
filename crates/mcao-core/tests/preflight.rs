use std::ffi::OsString;
use std::path::Path;

use mcao_core::preflight::{
    CLASSIC_PATH_LIMIT, EXTENDED_PATH_LIMIT, ONEDRIVE_RELOCATION, OneDriveCheck, PathCheck,
    PathLimit, TOOL_WARNING_LENGTH, check_home_outside_onedrive, check_path_len, default_home,
    projected_path_len, refuse_onedrive_home,
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
fn the_default_home_is_the_mcao_home_under_localappdata() {
    let local = host("/home/u/AppData/Local");
    let home = default_home(only("LOCALAPPDATA", &local)).expect("LOCALAPPDATA is set");

    assert!(home.starts_with(Path::new(&local)), "{}", home.display());
    assert!(
        home.ends_with(Path::new("mcao").join("home")),
        "{}",
        home.display()
    );
}

#[test]
fn the_default_home_needs_an_absolute_localappdata() {
    for value in ["", "AppData/Local"] {
        let error = default_home(only("LOCALAPPDATA", value)).expect_err("not absolute");
        assert_eq!(error.exit_code(), 3);
        assert!(error.to_string().contains("set FM_HOME"), "{error}");
    }
    let error = default_home(|_| None).expect_err("unset");
    assert!(
        error.to_string().contains("not set to an absolute path"),
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
fn the_onedrive_root_matches_ascii_case_only_for_windows_names() {
    let result = check_home_outside_onedrive(
        Path::new(&host("/home/u/onedrive/fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    );

    // Windows names compare ASCII-case-insensitively; unix names compare exactly.
    assert_eq!(result.is_err(), cfg!(windows), "{result:?}");
}

#[test]
fn an_empty_onedrive_variable_names_no_root() {
    check_home_outside_onedrive(Path::new(&host("/home/u/fm")), only("OneDrive", ""))
        .expect("an empty root must not match every home");
}

#[test]
fn a_home_under_the_profile_onedrive_folder_warns_without_a_root_variable() {
    let check = check_home_outside_onedrive(
        Path::new(&host("/home/u/OneDrive/Documents/fm")),
        only("USERPROFILE", &host("/home/u")),
    )
    .expect("a name alone does not refuse");
    let message = match check {
        OneDriveCheck::Warning(message) => message,
        other => panic!("expected a warning, got {other:?}"),
    };

    assert!(message.contains("beneath USERPROFILE"), "{message}");
    assert!(message.contains(ONEDRIVE_RELOCATION), "{message}");
}

#[test]
fn a_business_sync_folder_beneath_the_profile_warns() {
    let check = check_home_outside_onedrive(
        Path::new(&host("/home/u/OneDrive - Contoso/fm")),
        only("USERPROFILE", &host("/home/u")),
    )
    .expect("a name alone does not refuse");

    assert!(
        matches!(&check, OneDriveCheck::Warning(message) if message.contains("OneDrive - Contoso")),
        "{check:?}"
    );
}

#[test]
fn a_profile_sync_folder_that_a_root_also_names_is_refused() {
    let profile = host("/home/u");
    let env = |name: &str| match name {
        "OneDrive" => Some(OsString::from(host("/home/u/OneDrive"))),
        "USERPROFILE" => Some(OsString::from(&profile)),
        _ => None,
    };

    let error = check_home_outside_onedrive(Path::new(&host("/home/u/OneDrive/fm")), env)
        .expect_err("the root corroborates the sync folder");

    assert_eq!(error.exit_code(), 3);
    assert!(error.to_string().contains("named by OneDrive"), "{error}");
}

#[test]
fn the_profile_sync_folder_name_matches_case_only_for_windows_names() {
    let result = check_home_outside_onedrive(
        Path::new(&host("/home/u/onedrive/fm")),
        only("USERPROFILE", &host("/home/u")),
    );

    // Windows names compare ASCII-case-insensitively; unix names compare exactly.
    assert_eq!(
        matches!(result, Ok(OneDriveCheck::Warning(_))),
        cfg!(windows),
        "{result:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_windows_form_profile_or_root_does_not_refuse_a_unix_home() {
    check_home_outside_onedrive(Path::new("/home/u/fm"), only("USERPROFILE", r"C:\Users\u"))
        .expect("a Windows-form profile cannot contain a unix home");
    check_home_outside_onedrive(
        Path::new("/home/u/OneDrive/fm"),
        only("OneDrive", r"C:\Users\u\OneDrive"),
    )
    .expect("a Windows-form root cannot contain a unix home");
}

#[cfg(unix)]
#[test]
fn a_literal_backslash_in_a_unix_path_is_not_a_separator() {
    let env = |name: &str| match name {
        "OneDrive" => Some(OsString::from("/home/u/OneDrive")),
        "USERPROFILE" => Some(OsString::from("/home/u")),
        _ => None,
    };

    let check = check_home_outside_onedrive(Path::new(r"/home/u/OneDrive\fm"), env)
        .expect("one component named OneDrive\\fm");

    assert_eq!(check, OneDriveCheck::Clear);
}

#[test]
fn other_folders_beneath_the_profile_pass() {
    let profile = host("/home/u");
    for home in [
        "/home/u/OneDriveBackup/fm",
        "/home/u/Documents/OneDrive/fm",
        "/home/u/fm",
    ] {
        check_home_outside_onedrive(Path::new(&host(home)), only("USERPROFILE", &profile))
            .unwrap_or_else(|error| panic!("{home} passes: {error}"));
    }
}

#[test]
fn a_relative_user_profile_is_refused_when_the_home_is_absolute() {
    let error = check_home_outside_onedrive(
        Path::new(&host("/home/u/fm")),
        only("USERPROFILE", "Users/u"),
    )
    .expect_err("USERPROFILE cannot be compared");

    assert_eq!(error.exit_code(), 3);
    assert!(error.to_string().contains("USERPROFILE"), "{error}");
}

#[test]
fn a_stateful_operation_refuses_a_home_under_onedrive_with_exit_four() {
    let error = refuse_onedrive_home(
        Path::new(&host("/home/u/OneDrive/fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect_err("refused");

    assert_eq!(error.exit_code(), 4);
    assert!(error.to_string().contains(ONEDRIVE_RELOCATION), "{error}");
}

#[test]
fn a_stateful_operation_passes_a_home_in_a_profile_sync_folder_with_a_warning() {
    refuse_onedrive_home(
        Path::new(&host("/home/u/OneDrive/fm")),
        only("USERPROFILE", &host("/home/u")),
    )
    .expect("a warning does not refuse a stateful operation");
}

#[test]
fn a_stateful_operation_passes_a_home_outside_onedrive() {
    refuse_onedrive_home(
        Path::new(&host("/home/u/fm")),
        only("USERPROFILE", &host("/home/u")),
    )
    .expect("a home outside every sync folder");
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
fn a_dotdot_that_climbs_back_under_the_root_is_refused() {
    // The textual form is not under OneDrive, but `..` resolves it to `.../OneDrive/fm`.
    let error = check_home_outside_onedrive(
        Path::new(&host("/home/u/Other/../OneDrive/fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect_err("the resolved home is under the root");

    assert_eq!(error.exit_code(), 3);
    assert!(error.to_string().contains("named by OneDrive"), "{error}");
}

#[test]
fn a_dotdot_that_leaves_the_root_is_honored() {
    // `..` resolves the explicitly selected home to `.../Other/fm`, outside the root.
    check_home_outside_onedrive(
        Path::new(&host("/home/u/OneDrive/../Other/fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect("the resolved home is outside OneDrive");
}

#[test]
fn a_parent_component_above_the_root_stays_at_the_root() {
    let error = check_home_outside_onedrive(
        Path::new(&host("/../home/u/OneDrive/fm")),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect_err("a `..` above the root resolves to the root");

    assert!(error.to_string().contains("named by OneDrive"), "{error}");
}

#[test]
fn a_relative_home_whose_parent_components_cannot_resolve_is_refused() {
    for home in ["Sync/../mcao", "../Sync/mcao"] {
        let error = check_home_outside_onedrive(
            Path::new(home),
            only("OneDrive", &host("/home/u/OneDrive")),
        )
        .expect_err("a relative home cannot be compared");

        assert!(error.to_string().contains("cannot be checked"), "{error}");
    }
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

#[test]
fn a_verbatim_home_and_root_are_compared_without_their_prefix() {
    let verbatim_home = format!(r"\\?\{}", host("/home/u/OneDrive/fm"));
    let error = check_home_outside_onedrive(
        Path::new(&verbatim_home),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect_err("the plain form is under the root");
    assert!(error.to_string().contains("named by OneDrive"), "{error}");

    let verbatim_root = format!(r"\\?\{}", host("/home/u/OneDrive"));
    let error = check_home_outside_onedrive(
        Path::new(&host("/home/u/OneDrive/fm")),
        only("OneDrive", &verbatim_root),
    )
    .expect_err("the plain form of the root contains the home");
    assert!(error.to_string().contains("named by OneDrive"), "{error}");
}

#[test]
fn a_device_namespace_home_is_refused_not_compared() {
    let error = check_home_outside_onedrive(
        Path::new(r"\\.\C:\home\u\fm"),
        only("OneDrive", &host("/home/u/OneDrive")),
    )
    .expect_err("a device path cannot be compared");

    assert_eq!(error.exit_code(), 3);
    assert!(error.to_string().contains("cannot be checked"), "{error}");
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
