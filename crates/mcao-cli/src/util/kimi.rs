//! `mcao util kimi-turnend <install|remove> <kimi-config-dir>`: the validation and text edit behind
//! `bin/fm-kimi-turnend-hook.sh`, a port of its python3 program. The config is validated with
//! TOML 1.0 and edited as bytes, so everything outside the Firstmate-owned region is kept.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const BEGIN: &[u8] = b"# BEGIN FIRSTMATE KIMI TURN-END HOOK";
const BEGIN_OWNS_NEWLINE: &[u8] = b"# BEGIN FIRSTMATE KIMI TURN-END HOOK (OWNS PRECEDING NEWLINE)";
const END: &[u8] = b"# END FIRSTMATE KIMI TURN-END HOOK";
const IDENTIFIER: &[u8] = b"FIRSTMATE KIMI TURN-END HOOK";
const HOOK_NAME: &[u8] = b"fm-turn-end.sh";
const USAGE: &str = "usage: mcao util kimi-turnend <install|remove> <kimi-config-dir>";

const HOOK_BYTES: &[u8] = br##"#!/usr/bin/env bash
# Firstmate Kimi turn-end hook. Managed by fm-kimi-turnend-hook.sh.
# This hook is deliberately passive: every path is silent and exits zero.
set +e
exec >/dev/null 2>&1
payload=
IFS= read -r payload || [ -n "$payload" ] || exit 0
command -v jq >/dev/null 2>&1 || exit 0
workspace=$(jq -er 'select(.hook_event_name == "Stop") | .cwd | strings | select(length > 0)' <<< "$payload" 2>/dev/null) || exit 0
pointer="$workspace/.fm-kimi-turnend"
[ -f "$pointer" ] || exit 0
first=
IFS= read -r -n 256 first < "$pointer" 2>/dev/null || [ -n "$first" ] || exit 0
case "$first" in token=*) token=${first#token=} ;; *) exit 0 ;; esac
case "$token" in fm.????????????) : ;; *) exit 0 ;; esac
case "$token" in *[!A-Za-z0-9._-]*) exit 0 ;; esac
auth_dir=${HOME:-}/.kimi-code/fm-turn-end.d
[ -n "${HOME:-}" ] || exit 0
target=$(cat "$auth_dir/$token" 2>/dev/null) || exit 0
case "$target" in /*.turn-ended) : ;; *) exit 0 ;; esac
touch -- "$target" 2>/dev/null || true
exit 0
"##;

/// The Firstmate-owned span of the config: `start..after` in bytes, and whether its marker
/// claims the newline before it.
struct Region {
    start: usize,
    after: usize,
    owned: bool,
}

pub fn run(args: &[String]) -> ExitCode {
    match args {
        [action, dir] if action == "install" || action == "remove" => {
            match apply(action == "install", Path::new(dir)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(reason) => {
                    eprintln!("fm-kimi-turnend-hook: refused: {reason}");
                    ExitCode::from(1)
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn apply(install: bool, dir: &Path) -> Result<(), String> {
    let config_path = dir.join("config.toml");
    let hook = dir.join("fm-turn-end.sh");
    let registry = dir.join("fm-turn-end.d");

    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        _ => {
            return Err(format!(
                "Kimi config directory is missing or unexpected at {}.",
                dir.display()
            ));
        }
    }
    let config_meta = regular_file(&config_path, "Kimi config")?;
    let config_mode = mode_of(&config_meta);
    let original = fs::read(&config_path).map_err(fs_error)?;
    parse_toml(&original, "config.toml")?;
    let region = locate_region(&original)?;
    let outside = match &region {
        None => original.clone(),
        Some(region) => without_region(&original, region),
    };
    if find(&outside, HOOK_NAME).is_some() {
        return Err(
            "config.toml references fm-turn-end.sh outside the Firstmate-owned region.".into(),
        );
    }

    if install {
        if fs::symlink_metadata(&registry)
            .is_ok_and(|meta| meta.file_type().is_symlink() || !meta.is_dir())
        {
            return Err(format!(
                "Firstmate registry is not a regular directory at {}.",
                registry.display()
            ));
        }
        if fs::symlink_metadata(&hook).is_ok() {
            regular_file(&hook, "Firstmate hook script")?;
            let existing = fs::read(&hook).map_err(fs_error)?;
            if existing != HOOK_BYTES
                && !existing.starts_with(b"#!/usr/bin/env bash\n# Firstmate Kimi turn-end hook.")
            {
                return Err(format!(
                    "Firstmate hook path has unexpected content at {}.",
                    hook.display()
                ));
            }
        }
        let candidate = install_candidate(&original, region.as_ref());
        parse_toml(&candidate, "updated config.toml")?;
        match fs::DirBuilder::new().mode(0o700).create(&registry) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(fs_error(err)),
        }
        fs::set_permissions(&registry, fs::Permissions::from_mode(0o700)).map_err(fs_error)?;
        let installed = fs::read(&hook).ok();
        let hook_mode_ok = fs::metadata(&hook).is_ok_and(|meta| mode_of(&meta) == 0o700);
        if installed.as_deref() != Some(HOOK_BYTES) || !hook_mode_ok {
            atomic_write(&hook, HOOK_BYTES, 0o700)?;
        }
        if candidate != original {
            atomic_write(&config_path, &candidate, config_mode)?;
        }
    } else {
        validate_for_remove(&hook, &registry)?;
        parse_toml(&outside, "config.toml after Firstmate region removal")?;
        if outside != original {
            atomic_write(&config_path, &outside, config_mode)?;
        }
        if fs::symlink_metadata(&hook).is_ok() {
            fs::remove_file(&hook).map_err(fs_error)?;
        }
        if fs::symlink_metadata(&registry).is_ok() {
            fs::remove_dir_all(&registry).map_err(fs_error)?;
        }
    }
    Ok(())
}

fn install_candidate(original: &[u8], region: Option<&Region>) -> Vec<u8> {
    match region {
        None => {
            let ends_with_newline = original.ends_with(b"\n");
            let mut candidate = original.to_vec();
            if !ends_with_newline {
                candidate.push(b'\n');
            }
            let marker = if ends_with_newline {
                BEGIN
            } else {
                BEGIN_OWNS_NEWLINE
            };
            candidate.extend(block(marker));
            candidate
        }
        Some(region) => {
            let mut candidate = original[..region.start].to_vec();
            let marker = if region.owned {
                BEGIN_OWNS_NEWLINE
            } else {
                BEGIN
            };
            if region.owned {
                candidate.push(b'\n');
            }
            candidate.extend(block(marker));
            candidate.extend_from_slice(&original[region.after..]);
            candidate
        }
    }
}

fn validate_for_remove(hook: &Path, registry: &Path) -> Result<(), String> {
    if fs::symlink_metadata(hook).is_ok() {
        let meta = regular_file(hook, "Firstmate hook script")?;
        if fs::read(hook).map_err(fs_error)? != HOOK_BYTES {
            return Err(format!(
                "Firstmate hook script has unexpected content at {}.",
                hook.display()
            ));
        }
        if mode_of(&meta) & 0o077 != 0 {
            return Err(format!(
                "Firstmate hook script has unexpectedly broad permissions at {}.",
                hook.display()
            ));
        }
    }
    if fs::symlink_metadata(registry).is_ok() {
        let meta = fs::symlink_metadata(registry).map_err(fs_error)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(format!(
                "Firstmate registry is not a regular directory at {}.",
                registry.display()
            ));
        }
        for entry in fs::read_dir(registry).map_err(fs_error)? {
            let entry = entry.map_err(fs_error)?;
            let path = entry.path();
            let child = fs::symlink_metadata(&path).map_err(fs_error)?;
            let valid_name = entry.file_name().to_str().is_some_and(is_token_name);
            if !valid_name || child.file_type().is_symlink() || !child.is_file() {
                return Err(format!(
                    "Firstmate registry contains an unexpected entry at {}.",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

/// `fm\.[A-Za-z0-9]{12}` as a whole name.
fn is_token_name(name: &str) -> bool {
    name.strip_prefix("fm.")
        .is_some_and(|rest| rest.len() == 12 && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

fn regular_file(path: &Path, label: &str) -> Result<fs::Metadata, String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(format!("{label} is missing at {}.", path.display()));
        }
        Err(err) => return Err(fs_error(err)),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!(
            "{label} is not a regular non-symlink file at {}.",
            path.display()
        ));
    }
    Ok(meta)
}

fn parse_toml(data: &[u8], label: &str) -> Result<(), String> {
    let text = std::str::from_utf8(data).map_err(|err| format!("{label} is not UTF-8: {err}."))?;
    let table: toml::Table =
        toml::from_str(text).map_err(|err| format!("{label} is malformed TOML: {err}."))?;
    if table.get("hooks").is_some_and(|hooks| !hooks.is_array()) {
        return Err(format!(
            "{label} has an unexpected non-array 'hooks' value."
        ));
    }
    Ok(())
}

fn locate_region(data: &[u8]) -> Result<Option<Region>, String> {
    let begin_line = [BEGIN, b"\n"].concat();
    let owned_line = [BEGIN_OWNS_NEWLINE, b"\n"].concat();
    let normal_count = count(data, &begin_line);
    let owned_count = count(data, &owned_line);
    let end_count = count(data, END);
    let identifier_count = count(data, IDENTIFIER);
    if normal_count + owned_count == 0 && end_count == 0 && identifier_count == 0 {
        return Ok(None);
    }
    if normal_count + owned_count != 1 || end_count != 1 || identifier_count != 2 {
        return Err(
            "config.toml has partial, duplicated, or altered Firstmate region markers.".into(),
        );
    }
    let owned = owned_count == 1;
    let marker = if owned { BEGIN_OWNS_NEWLINE } else { BEGIN };
    let marker_at = find(data, marker)
        .ok_or("config.toml has partial, duplicated, or altered Firstmate region markers.")?;
    if marker_at != 0 && data[marker_at - 1] != b'\n' {
        return Err("the Firstmate begin marker is not at a line boundary.".into());
    }
    let mut start = marker_at;
    if owned {
        if marker_at == 0 || data[marker_at - 1] != b'\n' {
            return Err("the Firstmate region claims a preceding newline that is absent.".into());
        }
        start -= 1;
    }
    let end_at = find_from(data, END, marker_at + marker.len())
        .ok_or("the Firstmate end marker is missing.")?;
    let mut after = end_at + END.len();
    if after < data.len() {
        if data[after] != b'\n' {
            return Err("the Firstmate end marker is not a complete line.".into());
        }
        after += 1;
    }
    Ok(Some(Region {
        start,
        after,
        owned,
    }))
}

fn without_region(data: &[u8], region: &Region) -> Vec<u8> {
    let suffix = &data[region.after..];
    let mut out = data[..region.start].to_vec();
    // Keep a newline so removal cannot join the preceding line to content appended after install.
    if region.owned && !suffix.is_empty() {
        out.push(b'\n');
    }
    out.extend_from_slice(suffix);
    out
}

fn block(marker: &[u8]) -> Vec<u8> {
    let lines: [&[u8]; 8] = [
        marker,
        b"[[hooks]]",
        b"event = \"Stop\"",
        b"matcher = \"^$\"",
        br##"command = "bash \"$HOME/.kimi-code/fm-turn-end.sh\" >/dev/null 2>&1 || true""##,
        b"timeout = 1",
        END,
        b"",
    ];
    lines.join(&b'\n')
}

fn atomic_write(path: &Path, data: &[u8], mode: u32) -> Result<(), String> {
    let base = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let (temp, mut file) = create_temp(path, &base)?;
    let written = file
        .set_permissions(fs::Permissions::from_mode(mode))
        .and_then(|()| file.write_all(data))
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temp, path));
    if let Err(err) = written {
        let _ = fs::remove_file(&temp);
        return Err(fs_error(err));
    }
    Ok(())
}

fn create_temp(path: &Path, base: &str) -> Result<(PathBuf, fs::File), String> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    for attempt in 0..1000 {
        let temp = dir.join(format!(".{base}.{}.{attempt}", std::process::id()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
        {
            Ok(file) => return Ok((temp, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(fs_error(err)),
        }
    }
    Err("filesystem operation failed: no free temporary file name.".into())
}

fn mode_of(meta: &fs::Metadata) -> u32 {
    meta.permissions().mode() & 0o7777
}

fn fs_error(err: io::Error) -> String {
    format!("filesystem operation failed: {err}.")
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    let mut total = 0;
    let mut from = 0;
    while let Some(at) = find_from(haystack, needle, from) {
        total += 1;
        from = at + needle.len();
    }
    total
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    find_from(haystack, needle, 0)
}

fn find_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}
