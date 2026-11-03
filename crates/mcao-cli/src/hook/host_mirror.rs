//! `mcao hook host-mirror`: the writer half of the supervision host's dialog mirror (DPES-894).
//!
//! Ports the `hook` path of `bin/fm-host-mirror.sh`, the shipped writer contract. A prompt-submit
//! or turn-end payload on stdin becomes one JSON line in `state/.host-mirror.jsonl`, with the
//! record shape, text rules, cap, id dedupe, sequence numbering, and trim of that script. This port
//! is unwired: no harness registration calls it, so `.claude/settings.json` still runs the shell
//! writer.
//!
//! Not ported in this slice, so this writer records nothing the shell would refuse:
//! - The home gate (`fm_supervision_host_enabled`) and the primary-scope and session-lock gate
//!   (`writer_in_scope`). The state directory is resolved as `subagent-pretool` resolves it, and a
//!   missing directory is a silent no-op.
//! - The operational-input filter (`bin/fm-operational-input.sh classify`). Operational prompts are
//!   recorded here.
//! - The main-session key. The caller passes it with `--main-key`, and without it nothing is
//!   recorded. The shell derives it from the lock owner and the session lock.
//! - `feed`, `commit`, `check`, and `verified`. The shell remains the reader of the mirror file.
//!
//! The lock is the shell's `$STATE/.host-mirror.lock` directory protocol (`fm_lock_try_acquire` in
//! `bin/fm-wake-lib.sh`), so a shell writer and this one exclude each other. The lock is a symlink
//! to an owner directory holding the holder's `pid`. A writer waits for a live holder, up to the
//! `LOCK_WAIT` ceiling, and removes its own link on release. A stale holder is reclaimed as the
//! shell does it: under the `.host-mirror.lock.steal` mutex, with a recheck of the holder after the
//! mutex is taken, and a link that appears while a steal mutex is held, other than the reclaimer's,
//! is removed. A dangling steal link is held, and a link with no readable pid is held while it is
//! younger than `LOCK_MID_ACQUIRE`. The divergences:
//! - Liveness uses `kill(pid, 0)`, and `EPERM` counts as alive. The shell reads `EPERM` as dead and
//!   would reclaim another user's live lock. The shell fix is proposed as follow-up, not made here.
//! - The shell waits for a live holder without a bound. This writer gives up at the `LOCK_WAIT`
//!   ceiling, so a wedged holder cannot hang a prompt-time hook. The append is then dropped, which
//!   the best-effort contract permits.
//! - Not ported: the shell's nested `.steal.steal` reap, which only the former recursive reclaim
//!   wrote, and the plain-directory lock form that older shells created. This build does not
//!   reclaim either form, so a lock in either form is held until the `LOCK_WAIT` ceiling.
//! - A non-Unix build has no symlink lock, so every append returns an error and records nothing.
//!
//! Failures never fail the hook. A mirror that cannot be read or written, and a lock still held
//! at the `LOCK_WAIT` ceiling, make the internal writer return an error, and `run` discards it and
//! exits 0, as the shell hook exits 0 and prints nothing on every path.
//!
//! The omitted count of a capped text is the true count, the count the shell gives once its cap
//! reads the text through a file rather than `jq -Rs` stdin (DPES-894, `bin/fm-host-mirror.sh`).
//! A mirror that holds invalid UTF-8 is kept byte for byte, as the shell's append keeps it. Its
//! unparseable lines are skipped for the sequence and id checks, as `fromjson?` skips them, and the
//! shell's `feed` then refuses the file.
//!
//! Exit codes: 0 on every path. Standard output is always empty; standard error is empty except for
//! a refused OneDrive home, which names its relocation.

use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, ExitCode};
#[cfg(unix)]
use std::thread;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
#[cfg(unix)]
use std::time::{Duration, Instant};

use clap::Args;
use mcao_core::hook_source::payload_is_foreign_host;
use serde_json::Value;

use crate::home_guard::hook_home_refused;

const MIRROR_FILE: &str = ".host-mirror.jsonl";
const CURSOR_FILE: &str = ".host-mirror-cursor";
const STAGED_CURSOR_FILE: &str = ".host-mirror-cursor.next";
/// The shell's lock path. It is a symlink to an owner directory, as `fm_lock_try_create` makes it.
#[cfg(unix)]
const LOCK_FILE: &str = ".host-mirror.lock";
/// The defensive ceiling on the wait for a live holder. The shell waits without a bound; this
/// writer does not, so a wedged holder cannot hang a prompt-time hook.
#[cfg(unix)]
const LOCK_WAIT: Duration = Duration::from_secs(30);
/// Test-only override of `LOCK_WAIT` in milliseconds. Production never sets it.
#[cfg(unix)]
const LOCK_WAIT_ENV: &str = "MCAO_HOST_MIRROR_LOCK_WAIT_MS";
/// The poll interval, as the shell's `fm_lock_acquire_wait` sleeps between attempts.
#[cfg(unix)]
const LOCK_POLL: Duration = Duration::from_millis(100);
/// A link whose owner has no readable pid is still being acquired until it is this old, as
/// `fm_lock_mid_acquire_is_fresh` treats it with the shell's `FM_LOCK_STALE_AFTER` default.
#[cfg(unix)]
const LOCK_MID_ACQUIRE: Duration = Duration::from_secs(2);
/// Longest recorded text in characters, its truncation note included.
const MIRROR_CAP: usize = 4000;
/// A file that reaches this many lines is trimmed to its newest `MIRROR_KEEP` entries.
const MIRROR_KEEP: usize = 200;
const MIRROR_TRIM_AT: usize = MIRROR_KEEP + 100;
/// The on-disk `tag` of a user entry, spelled as the shell record schema spells it. `feed` filters
/// and prints this value, so it cannot be renamed here without breaking the shell reader.
const USER_TAG: &str = "captain";

/// Flags for `mcao hook host-mirror`. Like the shell's `hook <harness>`, a missing `--harness`
/// stands down with exit 0.
#[derive(Debug, Args)]
pub struct HostMirrorArgs {
    /// Harness that delivered the payload. Only `claude` changes the input check: its foreign-host
    /// payloads, which Cursor's loaded Claude settings deliver, are dropped.
    #[arg(long, value_name = "HARNESS")]
    harness: Option<String>,
    /// Main-session key the record is filed under. Without it nothing is recorded.
    #[arg(long, value_name = "KEY")]
    main_key: Option<String>,
    /// Firstmate root whose `state/` directory holds the mirror. Without it, `FM_ROOT_OVERRIDE`
    /// and then `CLAUDE_PROJECT_DIR` are read. The working directory is never used.
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,
}

/// One dialog entry the payload contributes.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    tag: &'static str,
    id: String,
    text: String,
}

/// Runs the writer. Always exits 0, as the shell hook does. A mirror that cannot be read or written,
/// or a lock still held after its bounded wait, records nothing. A refused OneDrive home prints its
/// relocation on standard error and records nothing.
pub fn run(args: &HostMirrorArgs) -> ExitCode {
    let Some(harness) = args.harness.as_deref() else {
        return ExitCode::SUCCESS;
    };
    let Some(root) = firstmate_root(args.root.as_deref()) else {
        return ExitCode::SUCCESS;
    };
    let state = state_dir(&root);
    if hook_home_refused() {
        return ExitCode::SUCCESS;
    }
    if !state.is_dir() {
        return ExitCode::SUCCESS;
    }
    let mut raw = Vec::new();
    if io::stdin().read_to_end(&mut raw).is_err() {
        return ExitCode::SUCCESS;
    }
    raw.retain(|&byte| byte != 0);
    if raw.is_empty() {
        return ExitCode::SUCCESS;
    }
    if harness == "claude" && payload_is_foreign_host(&raw) {
        return ExitCode::SUCCESS;
    }
    let Some(entry) = entry_from_payload(&String::from_utf8_lossy(&raw)) else {
        return ExitCode::SUCCESS;
    };
    let Some(key) = args.main_key.as_deref().filter(|key| !key.is_empty()) else {
        return ExitCode::SUCCESS;
    };
    // Best effort, as the shell hook is: the writer reports the failure, and the hook drops it.
    let _ = append_entry(&state, key, &entry);
    ExitCode::SUCCESS
}

/// Reads the dialog entry a hook payload carries, or `None` when the payload carries none. A
/// prompt-submit (`UserPromptSubmit`, `beforeSubmitPrompt`) is user text. A turn end (`Stop`,
/// `afterAgentResponse`) is main text. Trailing whitespace is trimmed, and empty text is dropped.
fn entry_from_payload(payload: &str) -> Option<Entry> {
    let payload = payload.strip_prefix('\u{feff}').unwrap_or(payload);
    let value: Value = serde_json::from_str(payload).ok()?;
    let fields = value.as_object()?;
    let field = |name: &str| fields.get(name);
    let first = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| truthy(field(name)))
            .map(jq_text)
            .unwrap_or_default()
    };
    let (tag, id, text) = match first(&["hook_event_name"]).as_str() {
        "UserPromptSubmit" | "beforeSubmitPrompt" => (
            USER_TAG,
            first(&["prompt_id", "generation_id"]),
            first(&["prompt"]),
        ),
        "Stop" => (
            "main",
            first(&["prompt_id", "generation_id"]),
            first(&["last_assistant_message"]),
        ),
        "afterAgentResponse" => ("main", first(&["generation_id"]), first(&["text"])),
        _ => return None,
    };
    let text = text.trim_end_matches(char::is_whitespace).to_owned();
    if text.is_empty() {
        return None;
    }
    Some(Entry { tag, id, text })
}

/// Matches jq's `//` falsiness: only `null` and `false` fall through to the next alternative.
fn truthy(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !matches!(value, Value::Null | Value::Bool(false)))
}

/// `(x | tostring)`: a string prints as itself, and any other value prints as its compact JSON.
fn jq_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Appends one entry under the shell's directory lock. A lock still held at the `LOCK_WAIT`
/// ceiling, or a mirror that cannot be read or written, is an error. A user entry the harness wrapped as a turn
/// it started itself is dropped.
fn append_entry(state: &Path, key: &str, entry: &Entry) -> io::Result<()> {
    // A turn the harness started itself arrives wrapped in this tag and is not user dialog.
    if entry.tag == USER_TAG && entry.text.trim_start().starts_with("<task-notification>") {
        return Ok(());
    }
    let state = fs::canonicalize(state)?;
    // The unix lock holds the mirror until the append returns, so its guard stays bound here.
    #[cfg(unix)]
    let _lock = acquire(&state)?;
    #[cfg(not(unix))]
    acquire(&state)?;
    append_locked(&state, key, entry)
}

fn append_locked(state: &Path, key: &str, entry: &Entry) -> io::Result<()> {
    let mirror = state.join(MIRROR_FILE);
    let existing = match fs::read(&mirror) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let text = capped(&entry.text);
    if !entry.id.is_empty()
        && existing.split(|&byte| byte == b'\n').any(|line| {
            serde_json::from_slice::<Value>(line).is_ok_and(|record| {
                record.get("id").and_then(Value::as_str) == Some(entry.id.as_str())
                    && record.get("tag").and_then(Value::as_str) == Some(entry.tag)
                    && record.get("key").and_then(Value::as_str) == Some(key)
                    && record.get("text").and_then(Value::as_str) == Some(text.as_str())
            })
        })
    {
        return Ok(());
    }
    let seq = next_seq(state, &existing);
    let record = format!(
        "{{\"seq\":{seq},\"epoch\":{},\"key\":{},\"id\":{},\"tag\":{},\"text\":{}}}",
        unix_epoch_seconds(),
        json_string(key),
        json_string(&entry.id),
        json_string(entry.tag),
        json_string(&text),
    );
    let kept = kept_lines(&existing);
    let tmp = state.join(format!("{MIRROR_FILE}.tmp.{}", process::id()));
    let written = write_file(&tmp, kept, record.as_bytes());
    if let Err(error) = written.and_then(|()| fs::rename(&tmp, &mirror)) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

/// The sequence after the newest of the mirror, the cursor, and the staged cursor, so a later
/// commit never skips new dialog.
fn next_seq(state: &Path, existing: &[u8]) -> u64 {
    let from_mirror = existing
        .split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter(Value::is_object)
        .filter_map(|record| record.get("seq").and_then(Value::as_u64))
        .max()
        .unwrap_or(0);
    [CURSOR_FILE, STAGED_CURSOR_FILE]
        .iter()
        .map(|name| cursor_seq(&state.join(name)))
        .fold(from_mirror, u64::max)
        + 1
}

/// The sequence in a cursor file's `<seq>\t<session>` first line, or 0 when it is absent or
/// malformed.
fn cursor_seq(path: &Path) -> u64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| {
            let line = text.lines().next()?;
            line.split('\t').next()?.parse::<u64>().ok()
        })
        .unwrap_or(0)
}

/// The mirror content to keep before the new record: all of it, or once it reaches
/// `MIRROR_TRIM_AT` lines, its newest `MIRROR_KEEP - 1` lines.
fn kept_lines(existing: &[u8]) -> &[u8] {
    let lines = existing.iter().filter(|&&byte| byte == b'\n').count();
    if lines < MIRROR_TRIM_AT {
        return existing;
    }
    let dropped = lines - (MIRROR_KEEP - 1);
    let keep_from = existing
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\n')
        .nth(dropped - 1)
        .map_or(0, |(index, _)| index + 1);
    &existing[keep_from..]
}

fn write_file(path: &Path, head: &[u8], record: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(head)?;
    file.write_all(record)?;
    file.write_all(b"\n")?;
    file.flush()
}

/// Caps a text at `MIRROR_CAP` characters, keeping its head and tail around a note that counts
/// the characters left out, as the shell's `capped` does. The count is the true one.
fn capped(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len <= MIRROR_CAP {
        return text.to_owned();
    }
    let inner = truncation_note(len - MIRROR_CAP).chars().count();
    let keep = MIRROR_CAP - truncation_note(len - MIRROR_CAP + inner).chars().count();
    let head = keep.div_ceil(2);
    let tail = keep / 2;
    let mut capped: String = chars[..head].iter().collect();
    capped.push_str(&truncation_note(len - keep));
    capped.extend(&chars[len - tail..]);
    capped
}

fn truncation_note(omitted: usize) -> String {
    format!("\n[mirror truncated: {omitted} characters omitted]\n")
}

/// A JSON string as `jq -c` prints it: serde_json's escaping, with DEL written as `\u007f`.
fn json_string(value: &str) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .replace('\u{7f}', "\\u007f")
}

fn unix_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Returns the firstmate root from an explicit input: `--root`, then `FM_ROOT_OVERRIDE`, then
/// `CLAUDE_PROJECT_DIR`. The working directory is never consulted.
fn firstmate_root(explicit: Option<&Path>) -> Option<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .or_else(|| non_empty_env("FM_ROOT_OVERRIDE").map(PathBuf::from))
        .or_else(|| non_empty_env("CLAUDE_PROJECT_DIR").map(PathBuf::from))
}

/// Returns the state directory: `FM_STATE_OVERRIDE`, else `state` under `FM_HOME`, which
/// defaults to `FM_ROOT_OVERRIDE` and then to the firstmate root.
fn state_dir(root: &Path) -> PathBuf {
    if let Some(state) = non_empty_env("FM_STATE_OVERRIDE") {
        return PathBuf::from(state);
    }
    let home = non_empty_env("FM_HOME")
        .or_else(|| non_empty_env("FM_ROOT_OVERRIDE"))
        .unwrap_or_else(|| root.as_os_str().to_owned());
    PathBuf::from(home).join("state")
}

fn non_empty_env(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

/// A held `$STATE/.host-mirror.lock`. Dropping it removes the link and the owner directory it
/// names, when both are still this process's.
#[cfg(unix)]
struct MirrorLock {
    link: PathBuf,
    owner: PathBuf,
}

#[cfg(unix)]
impl Drop for MirrorLock {
    fn drop(&mut self) {
        let held = fs::read_link(&self.link).is_ok_and(|target| target == self.owner);
        if held && read_pid(&self.owner.join("pid")) == Some(process::id() as i32) {
            let _ = fs::remove_file(&self.link);
            discard_owner(&self.owner);
        }
    }
}

/// The bounded wait: `LOCK_WAIT`, or the millisecond count in `LOCK_WAIT_ENV` when a test sets it.
#[cfg(unix)]
fn lock_wait() -> Duration {
    non_empty_env(LOCK_WAIT_ENV)
        .and_then(|value| value.to_str()?.parse::<u64>().ok())
        .map_or(LOCK_WAIT, Duration::from_millis)
}

/// Takes the shell's lock, waiting for a live holder to release it up to the bounded wait.
#[cfg(unix)]
fn acquire(state: &Path) -> io::Result<MirrorLock> {
    let link = state.join(LOCK_FILE);
    let deadline = Instant::now() + lock_wait();
    loop {
        if let Some(lock) = try_acquire(&link)? {
            return Ok(lock);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the host mirror lock is held past its bounded wait",
            ));
        }
        thread::sleep(LOCK_POLL);
    }
}

/// The steal mutex that serializes reclaim of a stale lock, named as `fm_lock_try_acquire` names it.
#[cfg(unix)]
fn steal_path(link: &Path) -> PathBuf {
    let mut steal = link.as_os_str().to_owned();
    steal.push(".steal");
    PathBuf::from(steal)
}

/// One attempt, as the shell's `fm_lock_try_acquire`: create the link, or reclaim a holder that is
/// dead or is this process. A reclaim runs under the steal mutex and rechecks the holder after
/// taking it, so two reclaimers cannot both remove the same stale link.
#[cfg(unix)]
fn try_acquire(link: &Path) -> io::Result<Option<MirrorLock>> {
    if let Some(lock) = create_lock(link, None)? {
        return Ok(Some(lock));
    }
    let holder = read_pid(&link.join("pid"));
    if holder == Some(process::id() as i32) {
        remove_lock_path(link);
        return create_lock(link, None);
    }
    if holder_is_held(link, holder) {
        return Ok(None);
    }
    let steal = steal_path(link);
    let Some(steal_lock) = acquire_steal_mutex(&steal)? else {
        return Ok(None);
    };
    reclaim_under_steal(link, &steal_lock)
}

/// Whether a holder still holds the lock. A live pid does. A link with no readable pid is held
/// only while it is younger than `LOCK_MID_ACQUIRE`, as the shell reads a holder that has not
/// written its pid yet.
#[cfg(unix)]
fn holder_is_held(link: &Path, holder: Option<i32>) -> bool {
    match holder {
        Some(pid) => pid_alive(pid),
        None => fs::symlink_metadata(link)
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| !modified.elapsed().is_ok_and(|age| age >= LOCK_MID_ACQUIRE)),
    }
}

/// Takes the steal mutex, reaping a dead holder of it first, as `fm_lock_try_acquire_steal_mutex`.
#[cfg(unix)]
fn acquire_steal_mutex(steal: &Path) -> io::Result<Option<MirrorLock>> {
    if let Some(lock) = create_lock(steal, None)? {
        return Ok(Some(lock));
    }
    if read_pid(&steal.join("pid")) == Some(process::id() as i32) {
        remove_lock_path(steal);
    } else if fs::symlink_metadata(steal).is_ok() && !reap_dead_link(steal) {
        return Ok(None);
    }
    create_lock(steal, None)
}

/// Under the steal mutex, removes the primary link when its holder is still stale, then claims the
/// lock. The steal must still name this process's owner, as `fm_lock_claim` checks.
#[cfg(unix)]
fn reclaim_under_steal(link: &Path, steal_lock: &MirrorLock) -> io::Result<Option<MirrorLock>> {
    if fs::read_link(link).is_err() {
        return Ok(None);
    }
    let holder = read_pid(&link.join("pid"));
    if holder_is_held(link, holder) || !points_to(&steal_path(link), &steal_lock.owner) {
        return Ok(None);
    }
    remove_lock_path(link);
    create_lock(link, Some(&steal_lock.owner))
}

/// Reaps a link whose holder is dead, as `fm_lock_reap_dead_link`. A live owner is renamed aside
/// by one reaper, and a competing rename of the same path fails. A dangling link is reaped only
/// when a reaper that has since died left its renamed owner behind; otherwise it is held.
#[cfg(unix)]
fn reap_dead_link(link: &Path) -> bool {
    let Ok(owner) = fs::read_link(link) else {
        return false;
    };
    let token = if owner.is_dir() {
        if holder_is_held(link, read_pid(&owner.join("pid"))) {
            return false;
        }
        owner.clone()
    } else {
        let mut token = None;
        for tomb in reaped_tombs(&owner) {
            if tomb_reaper_pid(&tomb)
                .is_some_and(|pid| pid != process::id() as i32 && pid_alive(pid))
            {
                return false;
            }
            token = Some(tomb);
        }
        match token {
            Some(token) => token,
            None => return false,
        }
    };
    let tomb = reaped_path(&owner, process::id());
    if token != tomb && fs::rename(&token, &tomb).is_err() {
        return false;
    }
    remove_link_if_points_to(link, &owner);
    discard_owner(&tomb);
    true
}

/// The renamed-aside owners that reapers left beside `owner`, in name order.
#[cfg(unix)]
fn reaped_tombs(owner: &Path) -> Vec<PathBuf> {
    let (Some(parent), Some(name)) = (owner.parent(), owner.file_name()) else {
        return Vec::new();
    };
    let prefix = format!("{}.reaped.", name.to_string_lossy());
    let Ok(entries) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut tombs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(&prefix))
        })
        .collect();
    tombs.sort();
    tombs
}

/// The pid a renamed-aside owner names as its reaper, when its suffix is a pid.
#[cfg(unix)]
fn tomb_reaper_pid(tomb: &Path) -> Option<i32> {
    let name = tomb.file_name()?.to_string_lossy().into_owned();
    name.rsplit_once(".reaped.")?.1.parse().ok()
}

#[cfg(unix)]
fn reaped_path(owner: &Path, reaper: u32) -> PathBuf {
    let name = owner.file_name().unwrap_or_default().to_string_lossy();
    owner.with_file_name(format!("{name}.reaped.{reaper}"))
}

/// Removes a lock path that is a link and its owner directory, as `fm_lock_remove_path`.
#[cfg(unix)]
fn remove_lock_path(link: &Path) {
    if let Ok(owner) = fs::read_link(link) {
        let _ = fs::remove_file(link);
        discard_owner(&owner);
    }
}

#[cfg(unix)]
fn points_to(link: &Path, owner: &Path) -> bool {
    fs::read_link(link).is_ok_and(|target| target == owner)
}

/// Creates an owner directory holding this pid, then links the lock path to it. The link is the
/// atomic step: `EEXIST` means another holder owns the lock. A link made while a steal mutex exists,
/// other than the one `allowed_steal_owner` holds, is removed again, as `fm_lock_claim` does.
#[cfg(unix)]
fn create_lock(link: &Path, allowed_steal_owner: Option<&Path>) -> io::Result<Option<MirrorLock>> {
    let owner = owner_path(link);
    fs::create_dir(&owner)?;
    let claimed = fs::write(owner.join("pid"), format!("{}\n", process::id()))
        .and_then(|()| std::os::unix::fs::symlink(&owner, link));
    match claimed {
        Ok(()) if steal_blocks_claim(link, allowed_steal_owner) => {
            remove_link_if_points_to(link, &owner);
            discard_owner(&owner);
            Ok(None)
        }
        Ok(()) => Ok(Some(MirrorLock {
            link: link.to_path_buf(),
            owner,
        })),
        Err(error) => {
            discard_owner(&owner);
            if error.kind() == io::ErrorKind::AlreadyExists {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }
}

#[cfg(unix)]
fn owner_path(link: &Path) -> PathBuf {
    let name = link.file_name().unwrap_or_default().to_string_lossy();
    link.with_file_name(format!("{name}.owner.{}.{}", process::id(), unix_nanos()))
}

/// Whether a steal mutex exists that is not `allowed_steal_owner`'s, as `fm_lock_claim_blocked_by_steal`.
#[cfg(unix)]
fn steal_blocks_claim(link: &Path, allowed_steal_owner: Option<&Path>) -> bool {
    let steal = steal_path(link);
    if fs::symlink_metadata(&steal).is_err() {
        return false;
    }
    allowed_steal_owner.is_none_or(|owner| !points_to(&steal, owner))
}

#[cfg(unix)]
fn remove_link_if_points_to(link: &Path, owner: &Path) {
    if points_to(link, owner) {
        let _ = fs::remove_file(link);
    }
}

#[cfg(unix)]
fn discard_owner(owner: &Path) {
    let _ = fs::remove_file(owner.join("pid"));
    let _ = fs::remove_dir(owner);
}

/// The pid an owner directory records, or `None` when it is absent, unreadable, or not positive.
#[cfg(unix)]
fn read_pid(path: &Path) -> Option<i32> {
    let pid: i32 = fs::read_to_string(path).ok()?.trim().parse().ok()?;
    (pid > 0).then_some(pid)
}

/// Whether `pid` names a live process. `EPERM` means it exists under another user, so it is alive.
#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes the pid; it delivers nothing.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
}

/// The lock needs a symlink, which this build does not create, so no append can take it.
#[cfg(not(unix))]
fn acquire(_state: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "the host mirror lock needs a symlink, which this build does not create",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_prompt_keeps_its_id_and_trims_trailing_whitespace() {
        let entry = entry_from_payload(
            r#"{"hook_event_name":"UserPromptSubmit","prompt_id":"p1","prompt":"ship it \n\t"}"#,
        );
        assert_eq!(
            entry,
            Some(Entry {
                tag: USER_TAG,
                id: "p1".to_owned(),
                text: "ship it".to_owned(),
            })
        );
    }

    #[test]
    fn turn_end_events_record_main_text() {
        let stop = entry_from_payload(
            r#"{"hook_event_name":"Stop","generation_id":7,"last_assistant_message":"done"}"#,
        );
        assert_eq!(
            stop.map(|entry| (entry.tag, entry.id)),
            Some(("main", "7".to_owned()))
        );
        let after = entry_from_payload(
            r#"{"hook_event_name":"afterAgentResponse","generation_id":"g","text":"ok"}"#,
        );
        assert_eq!(after.map(|entry| entry.tag), Some("main"));
    }

    #[test]
    fn empty_unknown_and_non_object_payloads_carry_no_entry() {
        assert_eq!(
            entry_from_payload(r#"{"hook_event_name":"Stop","last_assistant_message":" \n"}"#),
            None
        );
        assert_eq!(
            entry_from_payload(r#"{"hook_event_name":"PreToolUse"}"#),
            None
        );
        assert_eq!(entry_from_payload(r#"["UserPromptSubmit"]"#), None);
        assert_eq!(entry_from_payload("{not-json"), None);
    }

    #[test]
    fn capped_keeps_head_and_tail_around_the_omitted_count() {
        let long = capped(&"a".repeat(4500));
        assert_eq!(long.chars().count(), MIRROR_CAP);
        assert!(long.contains("[mirror truncated: 544 characters omitted]"));
        let exact = "b".repeat(MIRROR_CAP);
        assert_eq!(capped(&exact), exact);
    }

    #[test]
    fn json_string_escapes_del_as_jq_does() {
        assert_eq!(
            json_string("a\u{7f}\u{1}\t\"\\"),
            r#""a\u007f\u0001\t\"\\""#
        );
    }

    /// A scratch state directory for a unit test, removed before reuse.
    #[cfg(unix)]
    fn unit_state(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("host-mirror-unit-{}-{name}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create unit state directory");
        dir
    }

    #[cfg(unix)]
    fn unit_entry(text: &str) -> Entry {
        Entry {
            tag: "main",
            id: String::new(),
            text: text.to_owned(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_mirror_is_an_error_and_records_nothing() {
        let state = unit_state("unreadable");
        fs::create_dir(state.join(MIRROR_FILE)).expect("make mirror a directory");
        assert!(append_locked(&state, "key", &unit_entry("lost")).is_err());
        assert!(state.join(MIRROR_FILE).is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn failed_temp_write_is_an_error_and_keeps_the_mirror() {
        use std::os::unix::fs::PermissionsExt;

        let state = unit_state("write-failure");
        let existing = b"{\"seq\":1,\"epoch\":1,\"key\":\"key\",\"id\":\"\",\"tag\":\"main\",\"text\":\"kept\"}\n";
        fs::write(state.join(MIRROR_FILE), existing).expect("seed mirror");
        fs::set_permissions(&state, fs::Permissions::from_mode(0o500)).expect("make read-only");
        let result = append_locked(&state, "key", &unit_entry("lost"));
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).expect("restore write");
        assert!(result.is_err());
        assert_eq!(
            fs::read(state.join(MIRROR_FILE)).expect("mirror still readable"),
            existing
        );
    }

    #[test]
    fn kept_lines_keeps_the_newest_entries_at_the_trim_threshold() {
        let existing: String = (1..=300).map(|seq| format!("line {seq}\n")).collect();
        let kept = kept_lines(existing.as_bytes());
        let kept = std::str::from_utf8(kept).expect("kept lines are text");
        assert_eq!(kept.lines().count(), MIRROR_KEEP - 1);
        assert!(kept.starts_with("line 102\n"));
        assert!(kept.ends_with("line 300\n"));
    }
}
