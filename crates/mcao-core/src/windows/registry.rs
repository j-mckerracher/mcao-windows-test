//! Editor for the user `Path` in the Windows registry.
//!
//! The user `PATH` is the `Path` value under `HKCU\Environment`. Edits apply the pure helpers in
//! [`crate::path_edit`] and write the value back only when its text changed. The value keeps its
//! registry type, because writing `REG_SZ` over a `REG_EXPAND_SZ` would stop entries such as
//! `%USERPROFILE%\bin` from expanding. An absent value is created as `REG_EXPAND_SZ`, the type
//! Windows uses for `Path`. A `Path` of any other type is refused and never overwritten.
//!
//! The logic runs against the [`PathStore`] trait, so the unit tests use an in-memory store on
//! every platform. The Win32 calls are `cfg(windows)`. The edit functions do not broadcast; a
//! caller that changes the value calls [`broadcast_setting_change`] so new processes see it.

use std::io;

use crate::path_edit::{add_entry, remove_entry};

#[cfg(windows)]
use std::ptr::{null, null_mut};

#[cfg(windows)]
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, LPARAM, SetLastError};
#[cfg(windows)]
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ,
    REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegQueryValueExW,
    RegSetValueExW,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};

/// Registry subkey under `HKCU` that holds the user environment, including the user `Path`.
pub const USER_ENVIRONMENT_SUBKEY: &str = "Environment";

/// Registry value name that holds the user `PATH`.
#[cfg(windows)]
const PATH_VALUE: &str = "Path";

/// The registry type of a `Path` value that the editor may change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// `REG_SZ`.
    Sz,
    /// `REG_EXPAND_SZ`.
    ExpandSz,
}

/// Storage for one `Path` value. The Windows implementation is [`RegistryStore`].
pub trait PathStore {
    /// Reads the value and its type, or `Ok(None)` if the value does not exist.
    ///
    /// A value of any type other than `REG_SZ` or `REG_EXPAND_SZ` is an error of kind
    /// [`io::ErrorKind::InvalidData`], so the editor cannot overwrite it.
    fn read(&self) -> io::Result<Option<(String, ValueKind)>>;

    /// Writes the value with the given type.
    fn write(&mut self, value: &str, kind: ValueKind) -> io::Result<()>;
}

/// Adds `dir` to the stored `Path`. Returns `true` if the value was written.
///
/// A `dir` that is already present is not written again. An absent value is created as
/// `REG_EXPAND_SZ`.
pub fn add_to_store<S: PathStore>(store: &mut S, dir: &str) -> io::Result<bool> {
    edit_store(store, |path| add_entry(path, dir))
}

/// Removes `dir` from the stored `Path`. Returns `true` if the value was written.
///
/// An absent value, or a `dir` that is not present, is left alone and not written.
pub fn remove_from_store<S: PathStore>(store: &mut S, dir: &str) -> io::Result<bool> {
    edit_store(store, |path| remove_entry(path, dir))
}

/// Applies `edit` to the stored `Path` and writes the result only if its text changed.
///
/// An absent value reads as empty, so an edit that changes nothing writes nothing. That covers
/// removing from a missing `Path` and adding a directory that `path_edit` refuses.
fn edit_store<S: PathStore>(store: &mut S, edit: impl FnOnce(&str) -> String) -> io::Result<bool> {
    let (current, kind) = store
        .read()?
        .unwrap_or_else(|| (String::new(), ValueKind::ExpandSz));
    let updated = edit(&current);
    if updated == current {
        return Ok(false);
    }
    store.write(&updated, kind)?;
    Ok(true)
}

/// Adds `dir` to the user `PATH` in `HKCU\Environment`. Returns `true` if the value was written.
///
/// `dir` is a `&str` where spec 5.5 names `&Path`. The stored value is decoded from UTF-16 into a
/// `String` and split on `;`, so a `&Path` would need the same fallible `to_str()` conversion
/// before any edit. Callers convert at the boundary.
///
/// This is a read-modify-write with no lock and assumes a single user. A write by another process
/// between the read and the write is lost.
///
/// Call [`broadcast_setting_change`] afterwards so processes started later see the change.
#[cfg(windows)]
pub fn add_to_user_path(dir: &str) -> io::Result<bool> {
    add_to_store(&mut RegistryStore::open(USER_ENVIRONMENT_SUBKEY)?, dir)
}

/// Removes `dir` from the user `PATH` in `HKCU\Environment`. Returns `true` if the value was
/// written.
///
/// `dir` is a `&str` where spec 5.5 names `&Path`, for the reason given on [`add_to_user_path`].
///
/// This is a read-modify-write with no lock and assumes a single user. A write by another process
/// between the read and the write is lost.
///
/// Call [`broadcast_setting_change`] afterwards so processes started later see the change.
#[cfg(windows)]
pub fn remove_from_user_path(dir: &str) -> io::Result<bool> {
    remove_from_store(&mut RegistryStore::open(USER_ENVIRONMENT_SUBKEY)?, dir)
}

/// Tells top-level windows that the environment changed, with `lParam` set to `Environment`.
///
/// `SMTO_ABORTIFHUNG` keeps a hung window from blocking the caller. The 2-second timeout applies
/// to each top-level window separately, so the total wait can exceed 2 seconds when several
/// windows are slow. A zero return, which includes a timeout, is reported with the code from
/// `GetLastError`.
#[cfg(windows)]
pub fn broadcast_setting_change() -> io::Result<()> {
    const TIMEOUT_MS: u32 = 2_000;
    let environment = to_wide(USER_ENVIRONMENT_SUBKEY);
    let mut result: usize = 0;
    // Clear the last error so a zero return reports this call's code, not a stale one.
    // SAFETY: `SetLastError` only sets this thread's last-error value.
    unsafe { SetLastError(ERROR_SUCCESS) };
    // SAFETY: `environment` is NUL-terminated and outlives the call, and `result` is a valid out
    // pointer.
    let ok = unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as LPARAM,
            SMTO_ABORTIFHUNG,
            TIMEOUT_MS,
            &mut result,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A `Path` value under `HKCU\<subkey>`, read and written through the registry.
#[cfg(windows)]
pub struct RegistryStore {
    key: HKEY,
    subkey: String,
}

#[cfg(windows)]
impl RegistryStore {
    /// Opens `HKCU\<subkey>` for reading and writing, creating the key if it is missing.
    pub fn open(subkey: &str) -> io::Result<Self> {
        let name = to_wide(subkey);
        let mut key: HKEY = null_mut();
        // SAFETY: `name` is NUL-terminated and outlives the call. The null class and security
        // pointers and the null disposition pointer are all optional.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                name.as_ptr(),
                0,
                null(),
                REG_OPTION_NON_VOLATILE,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                null(),
                &mut key,
                null_mut(),
            )
        };
        check(status)?;
        Ok(Self {
            key,
            subkey: subkey.to_owned(),
        })
    }

    fn invalid(&self, detail: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "HKCU\\{}\\{PATH_VALUE} {detail}; it was left unchanged",
                self.subkey
            ),
        )
    }
}

#[cfg(windows)]
impl Drop for RegistryStore {
    fn drop(&mut self) {
        // SAFETY: `key` came from a successful `RegCreateKeyExW` and is closed only here.
        unsafe {
            RegCloseKey(self.key);
        }
    }
}

#[cfg(windows)]
impl PathStore for RegistryStore {
    fn read(&self) -> io::Result<Option<(String, ValueKind)>> {
        let name = to_wide(PATH_VALUE);
        let mut reg_type: u32 = 0;
        let mut size: u32 = 0;
        // SAFETY: `name` is NUL-terminated. A null data pointer asks only for the type and size.
        let status = unsafe {
            RegQueryValueExW(
                self.key,
                name.as_ptr(),
                null(),
                &mut reg_type,
                null_mut(),
                &mut size,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(status)?;
        let kind = match reg_type {
            REG_SZ => ValueKind::Sz,
            REG_EXPAND_SZ => ValueKind::ExpandSz,
            other => {
                return Err(self.invalid(&format!(
                    "has registry type {other}, not REG_SZ or REG_EXPAND_SZ"
                )));
            }
        };

        let mut units = vec![0u16; (size as usize).div_ceil(2)];
        let mut len = (units.len() * 2) as u32;
        // SAFETY: `units` holds `len` bytes, so the API writes only within the buffer.
        let status = unsafe {
            RegQueryValueExW(
                self.key,
                name.as_ptr(),
                null(),
                null_mut(),
                units.as_mut_ptr().cast(),
                &mut len,
            )
        };
        check(status)?;
        if !len.is_multiple_of(2) {
            return Err(self.invalid("has an odd byte length"));
        }

        // Registry strings often end in NUL terminators, which are not part of the path.
        let mut end = len as usize / 2;
        while end > 0 && units[end - 1] == 0 {
            end -= 1;
        }
        let text =
            String::from_utf16(&units[..end]).map_err(|_| self.invalid("is not valid UTF-16"))?;
        Ok(Some((text, kind)))
    }

    fn write(&mut self, value: &str, kind: ValueKind) -> io::Result<()> {
        let name = to_wide(PATH_VALUE);
        let data: Vec<u8> = to_wide(value)
            .iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        let reg_type = match kind {
            ValueKind::Sz => REG_SZ,
            ValueKind::ExpandSz => REG_EXPAND_SZ,
        };
        // SAFETY: `data` holds `data.len()` bytes of NUL-terminated UTF-16, and `name` is
        // NUL-terminated.
        let status = unsafe {
            RegSetValueExW(
                self.key,
                name.as_ptr(),
                0,
                reg_type,
                data.as_ptr(),
                data.len() as u32,
            )
        };
        check(status)
    }
}

/// Encodes `text` as NUL-terminated UTF-16.
#[cfg(windows)]
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Converts a Win32 status code into an `io::Result`.
#[cfg(windows)]
fn check(status: u32) -> io::Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = r"C:\tools\a";
    const B: &str = r"C:\tools\b";

    /// An in-memory `Path` value that records every write.
    #[derive(Default)]
    struct FakeStore {
        value: Option<(String, ValueKind)>,
        read_error: bool,
        writes: Vec<(String, ValueKind)>,
    }

    impl FakeStore {
        fn holding(path: &str, kind: ValueKind) -> Self {
            Self {
                value: Some((path.to_owned(), kind)),
                ..Self::default()
            }
        }
    }

    impl PathStore for FakeStore {
        fn read(&self) -> io::Result<Option<(String, ValueKind)>> {
            if self.read_error {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "registry type is REG_DWORD",
                ));
            }
            Ok(self.value.clone())
        }

        fn write(&mut self, value: &str, kind: ValueKind) -> io::Result<()> {
            self.value = Some((value.to_owned(), kind));
            self.writes.push((value.to_owned(), kind));
            Ok(())
        }
    }

    #[test]
    fn add_appends_the_entry_with_one_write() {
        let mut store = FakeStore::holding(A, ValueKind::ExpandSz);
        assert!(add_to_store(&mut store, B).unwrap());
        assert_eq!(
            store.writes,
            vec![(format!("{A};{B}"), ValueKind::ExpandSz)]
        );
    }

    #[test]
    fn adding_twice_writes_once() {
        let mut store = FakeStore::holding(A, ValueKind::ExpandSz);
        assert!(add_to_store(&mut store, B).unwrap());
        assert!(!add_to_store(&mut store, B).unwrap());
        assert_eq!(store.writes.len(), 1);
    }

    #[test]
    fn remove_drops_only_the_matching_entry() {
        let path = format!(r"{A};{B};%USERPROFILE%\bin");
        let mut store = FakeStore::holding(&path, ValueKind::Sz);
        assert!(remove_from_store(&mut store, B).unwrap());
        assert_eq!(
            store.writes,
            vec![(format!(r"{A};%USERPROFILE%\bin"), ValueKind::Sz)]
        );
    }

    #[test]
    fn removing_an_absent_entry_writes_nothing() {
        let mut store = FakeStore::holding(A, ValueKind::ExpandSz);
        assert!(!remove_from_store(&mut store, B).unwrap());
        assert!(store.writes.is_empty());
    }

    #[test]
    fn removing_from_a_missing_value_writes_nothing() {
        let mut store = FakeStore::default();
        assert!(!remove_from_store(&mut store, B).unwrap());
        assert!(store.writes.is_empty());
        assert!(store.value.is_none());
    }

    #[test]
    fn expand_sz_is_preserved_on_write() {
        let mut store = FakeStore::holding(r"%USERPROFILE%\bin", ValueKind::ExpandSz);
        assert!(add_to_store(&mut store, A).unwrap());
        assert_eq!(store.writes[0].1, ValueKind::ExpandSz);
    }

    #[test]
    fn reg_sz_is_preserved_on_write() {
        let mut store = FakeStore::holding(A, ValueKind::Sz);
        assert!(add_to_store(&mut store, B).unwrap());
        assert_eq!(store.writes[0].1, ValueKind::Sz);
    }

    #[test]
    fn absent_value_is_created_as_expand_sz() {
        let mut store = FakeStore::default();
        assert!(add_to_store(&mut store, B).unwrap());
        assert_eq!(store.writes, vec![(B.to_owned(), ValueKind::ExpandSz)]);
    }

    #[test]
    fn read_error_is_returned_without_a_write() {
        let mut store = FakeStore {
            read_error: true,
            ..FakeStore::default()
        };
        assert_eq!(
            add_to_store(&mut store, B).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            remove_from_store(&mut store, B).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(store.writes.is_empty());
    }

    #[test]
    fn unusable_directory_never_writes() {
        let mut store = FakeStore::holding(A, ValueKind::ExpandSz);
        assert!(!add_to_store(&mut store, r"C:\x;C:\y").unwrap());
        assert!(store.writes.is_empty());
    }

    #[test]
    fn removing_the_last_entry_writes_an_empty_value() {
        let mut store = FakeStore::holding(A, ValueKind::ExpandSz);
        assert!(remove_from_store(&mut store, A).unwrap());
        assert_eq!(store.writes, vec![(String::new(), ValueKind::ExpandSz)]);
    }
}
