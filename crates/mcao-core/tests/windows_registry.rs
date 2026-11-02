//! Round trip of the user `PATH` editor against a throwaway `HKCU` key.
//!
//! These tests never touch `HKCU\Environment`. Each one deletes its key on drop, even when it fails.

#![cfg(windows)]

use std::io;
use std::iter::once;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicU32, Ordering};

use mcao_core::windows::registry::{
    PathStore, RegistryStore, ValueKind, add_to_store, remove_from_store,
};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE,
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

const A: &str = r"C:\tools\a";
const B: &str = r"C:\tools\b";

/// A unique `HKCU` subkey that is deleted on drop.
struct ThrowawayKey(String);

impl ThrowawayKey {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(format!(r"Software\mcao-test-{}-{n}", std::process::id()))
    }
}

impl Drop for ThrowawayKey {
    fn drop(&mut self) {
        let name = wide(&self.0);
        // SAFETY: `name` is NUL-terminated and outlives the call.
        unsafe {
            RegDeleteTreeW(HKEY_CURRENT_USER, name.as_ptr());
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(once(0)).collect()
}

/// Creates the throwaway key with a `REG_DWORD` value named `Path`.
fn write_dword_path(subkey: &str) {
    let name = wide(subkey);
    let value = wide("Path");
    let data = 7u32.to_le_bytes();
    let mut key: HKEY = null_mut();
    // SAFETY: every string is NUL-terminated and outlives its call, and `key` is closed below.
    unsafe {
        let created = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            name.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut key,
            null_mut(),
        );
        assert_eq!(created, ERROR_SUCCESS);
        let set = RegSetValueExW(key, value.as_ptr(), 0, REG_DWORD, data.as_ptr(), 4);
        assert_eq!(set, ERROR_SUCCESS);
        RegCloseKey(key);
    }
}

/// Reads the raw `Path` value under the throwaway key, returning its registry type and data.
fn read_raw_path(subkey: &str) -> (u32, Vec<u8>) {
    let name = wide(subkey);
    let value = wide("Path");
    let mut key: HKEY = null_mut();
    let mut reg_type: u32 = 0;
    let mut data = [0u8; 64];
    let mut len = data.len() as u32;
    // SAFETY: every string is NUL-terminated and outlives its call, `data` holds `len` bytes, and
    // `key` is closed below.
    unsafe {
        let opened = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            name.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        );
        assert_eq!(opened, ERROR_SUCCESS);
        let queried = RegQueryValueExW(
            key,
            value.as_ptr(),
            null(),
            &mut reg_type,
            data.as_mut_ptr(),
            &mut len,
        );
        assert_eq!(queried, ERROR_SUCCESS);
        RegCloseKey(key);
    }
    (reg_type, data[..len as usize].to_vec())
}

#[test]
fn round_trip_creates_then_edits_an_expand_sz_path() {
    let key = ThrowawayKey::new();
    let mut store = RegistryStore::open(&key.0).unwrap();
    assert!(store.read().unwrap().is_none());

    assert!(add_to_store(&mut store, A).unwrap());
    assert_eq!(
        store.read().unwrap(),
        Some((A.to_owned(), ValueKind::ExpandSz))
    );
    assert!(!add_to_store(&mut store, A).unwrap());
    assert!(remove_from_store(&mut store, A).unwrap());
    assert_eq!(
        store.read().unwrap(),
        Some((String::new(), ValueKind::ExpandSz))
    );
}

#[test]
fn reg_sz_path_keeps_its_type() {
    let key = ThrowawayKey::new();
    let mut store = RegistryStore::open(&key.0).unwrap();
    store.write(A, ValueKind::Sz).unwrap();

    assert!(add_to_store(&mut store, B).unwrap());
    assert_eq!(
        store.read().unwrap(),
        Some((format!("{A};{B}"), ValueKind::Sz))
    );
}

#[test]
fn dword_path_is_refused_and_left_alone() {
    let key = ThrowawayKey::new();
    write_dword_path(&key.0);
    let mut store = RegistryStore::open(&key.0).unwrap();

    assert_eq!(
        add_to_store(&mut store, A).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(store.read().unwrap_err().kind(), io::ErrorKind::InvalidData);

    let (reg_type, data) = read_raw_path(&key.0);
    assert_eq!(reg_type, REG_DWORD);
    assert_eq!(data, 7u32.to_le_bytes());
}
