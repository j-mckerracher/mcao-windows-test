//! Named-pipe server for the worker console (DPES-883).
//!
//! A task's pipe is `\\.\pipe\mcao-<hash8>-<task_id>`, where `hash8` is the first eight hex
//! digits of the SHA-256 of the canonical home path. Eight hex digits carry 32 bits, so two homes
//! can still map to the same name. [`pipe_name`] and the SID comparison are pure and build on
//! every platform.
//!
//! On Windows, [`PipeServer`] creates each instance with a DACL that grants read and write to
//! the current user's SID only, and rejects remote clients. The first instance is created with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE`, so [`PipeServer::create`] fails if the name already has an
//! instance. That flag does not stop another process from creating the name before the server
//! does. [`verify_client_acl`] checks that a connected client runs as the same user. Dropping a
//! [`PipeServer`] closes its listening instance, so once every connection is closed, a new
//! connection fails with `ERROR_FILE_NOT_FOUND`.
//!
//! Known limits:
//! - Post-start same-user squatting: `FILE_FLAG_FIRST_PIPE_INSTANCE` guards only the first
//!   instance. Later instances are created without it, and the DACL limits who may connect, not
//!   who may create an instance. Any process running as the same user can therefore create
//!   another instance of the name and receive a client that connects in the gap. The server
//!   does not defend against that. Code running as the same user is trusted.
//! - PID lookup: [`verify_client_acl`] finds the client with `GetNamedPipeClientProcessId`, then
//!   opens that PID and reads its token. A PID can be reused after the client exits, so the
//!   check reads whichever process holds the PID when `OpenProcess` runs. It is a guard against
//!   other users, not proof of identity. The DACL is the primary control.

use mcao_core::sha256;
use mcao_core::state::TaskId;

#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::fmt;
#[cfg(windows)]
use std::fs::File;
#[cfg(windows)]
use std::io;
#[cfg(windows)]
use std::iter;
#[cfg(windows)]
use std::mem;
#[cfg(windows)]
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
#[cfg(windows)]
use std::ptr;
#[cfg(windows)]
use std::slice;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, FALSE, GENERIC_READ, GENERIC_WRITE,
    HANDLE, INVALID_HANDLE_VALUE, TRUE,
};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, GetLengthSid, GetTokenInformation,
    InitializeAcl, InitializeSecurityDescriptor, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
    SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
#[cfg(windows)]
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, PeekNamedPipe,
};
#[cfg(windows)]
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

const PIPE_PREFIX: &str = r"\\.\pipe\mcao-";

/// Per-direction buffer size the kernel reserves for a pipe instance.
#[cfg(windows)]
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;

/// Returns the pipe name for `task_id` under the home whose canonical path is `canonical_home`.
pub fn pipe_name(canonical_home: &str, task_id: &TaskId) -> String {
    let hash = sha256::hex_digest(canonical_home.as_bytes());
    format!("{PIPE_PREFIX}{}-{}", &hash[..8], task_id.as_str())
}

/// Returns the length of the binary SID at the start of `sid`, or `None` if `sid` does not start
/// with a revision 1 SID whose sub-authorities fit in the buffer.
pub fn sid_len(sid: &[u8]) -> Option<usize> {
    const HEADER_LEN: usize = 8;
    const MAX_SUB_AUTHORITIES: u8 = 15;
    let &revision = sid.first()?;
    let &count = sid.get(1)?;
    if revision != 1 || count > MAX_SUB_AUTHORITIES {
        return None;
    }
    let len = HEADER_LEN + 4 * usize::from(count);
    (sid.len() >= len).then_some(len)
}

/// Reports whether two binary SIDs name the same principal.
///
/// Compares the full SID bytes, which agrees with `EqualSid` for well-formed SIDs.
pub fn sids_match(server: &[u8], client: &[u8]) -> bool {
    match (sid_len(server), sid_len(client)) {
        (Some(server_len), Some(client_len)) => server[..server_len] == client[..client_len],
        _ => false,
    }
}

/// Why a pipe operation failed.
#[cfg(windows)]
#[derive(Debug)]
pub enum PipeError {
    /// The client runs as a different user than the server, or its process cannot be opened to
    /// check, so it is refused.
    AccessDenied,
    /// A Windows call failed.
    Os {
        call: &'static str,
        source: io::Error,
    },
}

#[cfg(windows)]
impl fmt::Display for PipeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessDenied => f.write_str("access denied"),
            Self::Os { call, source } => write!(f, "{call} failed: {source}"),
        }
    }
}

#[cfg(windows)]
impl std::error::Error for PipeError {}

/// A named-pipe server whose instances admit only the user that created it.
///
/// `accept` keeps one listening instance. Dropping the server closes it.
#[cfg(windows)]
#[derive(Debug)]
pub struct PipeServer {
    name: Vec<u16>,
    user_sid: Vec<u8>,
    listener: OwnedHandle,
}

#[cfg(windows)]
impl PipeServer {
    /// Creates the pipe `name` with its first instance.
    ///
    /// Fails if the name already has an instance (`FILE_FLAG_FIRST_PIPE_INSTANCE`).
    pub fn create(name: &str) -> Result<Self, PipeError> {
        let user_sid = current_user_sid()?;
        let name: Vec<u16> = name.encode_utf16().chain(iter::once(0)).collect();
        let listener = create_instance(&name, &user_sid, true)?;
        Ok(Self {
            name,
            user_sid,
            listener,
        })
    }

    /// Waits for a client and returns its connection.
    ///
    /// The replacement instance is created before waiting, not after `ConnectNamedPipe` returns.
    /// Creating it afterwards would leave a window with no listening instance, where a client
    /// that connects fails with `ERROR_FILE_NOT_FOUND`. Creating it first keeps one listener
    /// available at all times, so a second client can connect while the first is still
    /// connected. If creating it fails, the listener stays in place and the error is returned.
    /// If the client connected and closed before this call (`ERROR_NO_DATA`), the listener is
    /// dead, so it is replaced and the error is returned.
    ///
    /// The returned client is not verified. Callers must run [`verify_client_acl`] on it before
    /// trusting it.
    pub fn accept(&mut self) -> Result<PipeClient, PipeError> {
        let next = create_instance(&self.name, &self.user_sid, false)?;
        // SAFETY: `listener` is a live pipe instance owned by `self`.
        if unsafe { ConnectNamedPipe(self.listener.as_raw_handle(), ptr::null_mut()) } == 0 {
            let source = io::Error::last_os_error();
            let code = source.raw_os_error();
            if code != Some(ERROR_PIPE_CONNECTED as i32) {
                if code == Some(ERROR_NO_DATA as i32) {
                    self.listener = next;
                }
                return Err(PipeError::Os {
                    call: "ConnectNamedPipe",
                    source,
                });
            }
        }
        let connected = mem::replace(&mut self.listener, next);
        Ok(PipeClient { handle: connected })
    }
}

/// A connected client of a [`PipeServer`]. Dropping it closes this connection.
#[cfg(windows)]
#[derive(Debug)]
pub struct PipeClient {
    handle: OwnedHandle,
}

#[cfg(windows)]
impl AsHandle for PipeClient {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}

#[cfg(windows)]
impl From<PipeClient> for File {
    fn from(client: PipeClient) -> Self {
        File::from(client.handle)
    }
}

/// Returns how many bytes a read on `pipe` would return now, without consuming them.
#[cfg(windows)]
pub fn bytes_available(pipe: BorrowedHandle<'_>) -> io::Result<u32> {
    let mut available = 0_u32;
    // SAFETY: `pipe` is valid for the call. Null buffer and message-length pointers are allowed
    // when only the available count is requested, and `available` is writable.
    if unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            &mut available,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

/// Checks that the client on the other end of `pipe` runs as the current user.
///
/// Returns [`PipeError::AccessDenied`] on a SID mismatch, and when the client process cannot be
/// opened with `ERROR_ACCESS_DENIED`. Any other error also means the client was not verified, so
/// the caller must refuse it the same way.
#[cfg(windows)]
pub fn verify_client_acl(pipe: BorrowedHandle<'_>) -> Result<(), PipeError> {
    let server_sid = current_user_sid()?;
    let mut client_pid = 0_u32;
    // SAFETY: `pipe` is a valid handle for the duration of the call, and `client_pid` is writable.
    if unsafe { GetNamedPipeClientProcessId(pipe.as_raw_handle(), &mut client_pid) } == 0 {
        return Err(os_error("GetNamedPipeClientProcessId"));
    }
    // SAFETY: OpenProcess has no pointer arguments; a null result signals failure.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, client_pid) };
    if process.is_null() {
        let source = io::Error::last_os_error();
        if source.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
            return Err(PipeError::AccessDenied);
        }
        return Err(PipeError::Os {
            call: "OpenProcess",
            source,
        });
    }
    // SAFETY: `process` is a non-null handle that OpenProcess returned to us.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `process` is valid and `token` is writable.
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(os_error("OpenProcessToken"));
    }
    // SAFETY: `token` is a non-null handle that OpenProcessToken just returned.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let client_sid = token_user_sid(token.as_raw_handle())?;
    if sids_match(&server_sid, &client_sid) {
        Ok(())
    } else {
        Err(PipeError::AccessDenied)
    }
}

#[cfg(windows)]
fn os_error(call: &'static str) -> PipeError {
    PipeError::Os {
        call,
        source: io::Error::last_os_error(),
    }
}

#[cfg(windows)]
fn current_user_sid() -> Result<Vec<u8>, PipeError> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: the pseudo-handle from GetCurrentProcess needs no closing, and `token` is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(os_error("OpenProcessToken"));
    }
    // SAFETY: `token` is a non-null handle that OpenProcessToken just returned.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_user_sid(token.as_raw_handle())
}

/// Copies the user SID out of `token` as its binary form.
#[cfg(windows)]
fn token_user_sid(token: HANDLE) -> Result<Vec<u8>, PipeError> {
    let mut needed = 0_u32;
    // SAFETY: a null buffer with zero length only asks for the size, so the failure is expected.
    unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed) };
    // A u64 buffer keeps the TOKEN_USER pointer fields aligned.
    let mut buf = vec![0_u64; (needed as usize).div_ceil(mem::size_of::<u64>())];
    let size = (buf.len() * mem::size_of::<u64>()) as u32;
    let mut returned = 0_u32;
    // SAFETY: `buf` holds `size` writable bytes, and the call writes a TOKEN_USER at its start.
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            size,
            &mut returned,
        )
    } == 0
    {
        return Err(os_error("GetTokenInformation"));
    }
    // SAFETY: the successful call above wrote a TOKEN_USER at the start of `buf`.
    let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    // SAFETY: `sid` points into `buf`, which is still alive, so the length read covers valid memory.
    let len = unsafe { GetLengthSid(sid) } as usize;
    // SAFETY: `sid` points to `len` bytes inside `buf`.
    Ok(unsafe { slice::from_raw_parts(sid.cast::<u8>(), len) }.to_vec())
}

/// Builds a DACL that grants read and write to `user_sid` and to nothing else.
#[cfg(windows)]
fn owner_only_acl(user_sid: &[u8]) -> Result<Vec<u64>, PipeError> {
    // ACCESS_ALLOWED_ACE already holds the first dword of its SID, so the SID adds only the rest.
    let len = mem::size_of::<ACL>() + mem::size_of::<ACCESS_ALLOWED_ACE>() - mem::size_of::<u32>()
        + user_sid.len();
    let mut buf = vec![0_u64; len.div_ceil(mem::size_of::<u64>())];
    let acl = buf.as_mut_ptr().cast::<ACL>();
    // SAFETY: `buf` holds at least `len` writable bytes, which fit the ACL and one ACE.
    if unsafe { InitializeAcl(acl, len as u32, ACL_REVISION) } == 0 {
        return Err(os_error("InitializeAcl"));
    }
    // SAFETY: `acl` is initialized, and the SID bytes stay alive for the call.
    if unsafe {
        AddAccessAllowedAce(
            acl,
            ACL_REVISION,
            GENERIC_READ | GENERIC_WRITE,
            user_sid.as_ptr().cast_mut().cast::<c_void>(),
        )
    } == 0
    {
        return Err(os_error("AddAccessAllowedAce"));
    }
    Ok(buf)
}

/// Creates one pipe instance whose DACL admits only `user_sid`.
#[cfg(windows)]
fn create_instance(name: &[u16], user_sid: &[u8], first: bool) -> Result<OwnedHandle, PipeError> {
    let acl = owner_only_acl(user_sid)?;
    // SAFETY: an all-zero SECURITY_DESCRIPTOR is a valid starting value that InitializeSecurityDescriptor overwrites.
    let mut descriptor: SECURITY_DESCRIPTOR = unsafe { mem::zeroed() };
    let descriptor_ptr = ptr::addr_of_mut!(descriptor).cast::<c_void>();
    // SAFETY: `descriptor_ptr` points to a writable descriptor of the right size.
    if unsafe { InitializeSecurityDescriptor(descriptor_ptr, SECURITY_DESCRIPTOR_REVISION) } == 0 {
        return Err(os_error("InitializeSecurityDescriptor"));
    }
    // SAFETY: `acl` stays alive until CreateNamedPipeW has copied the descriptor.
    if unsafe { SetSecurityDescriptorDacl(descriptor_ptr, TRUE, acl.as_ptr().cast::<ACL>(), FALSE) }
        == 0
    {
        return Err(os_error("SetSecurityDescriptorDacl"));
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor_ptr,
        bInheritHandle: FALSE,
    };
    let mut open_mode = PIPE_ACCESS_DUPLEX;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let pipe_mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: `name` is NUL-terminated and `attributes` and its descriptor stay alive for the call.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            pipe_mode,
            PIPE_UNLIMITED_INSTANCES,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(os_error("CreateNamedPipeW"));
    }
    // SAFETY: `handle` is a valid pipe instance that CreateNamedPipeW just returned.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str) -> TaskId {
        TaskId::parse_for_creation(id).expect("valid task id")
    }

    /// The binary form of `S-1-5-21-1-2-3-<rid>`.
    fn user_sid(rid: u32) -> Vec<u8> {
        let mut sid = vec![1, 5, 0, 0, 0, 0, 0, 5];
        for part in [21_u32, 1, 2, 3, rid] {
            sid.extend_from_slice(&part.to_le_bytes());
        }
        sid
    }

    #[test]
    fn pipe_name_uses_first_eight_hash_digits_and_task_id() {
        // SHA-256 of the empty string begins e3b0c442.
        assert_eq!(pipe_name("", &task("t-1")), r"\\.\pipe\mcao-e3b0c442-t-1");
    }

    #[test]
    fn pipe_name_differs_by_home_and_by_task() {
        let base = pipe_name(r"C:\homes\a", &task("t-1"));
        assert_ne!(base, pipe_name(r"C:\homes\b", &task("t-1")));
        assert_ne!(base, pipe_name(r"C:\homes\a", &task("t-2")));
    }

    #[test]
    fn sid_len_reads_declared_length_and_ignores_trailing_bytes() {
        assert_eq!(sid_len(&user_sid(1000)), Some(28));
        let mut padded = user_sid(1000);
        padded.push(0xff);
        assert_eq!(sid_len(&padded), Some(28));
    }

    #[test]
    fn sid_len_rejects_malformed_sids() {
        assert_eq!(sid_len(&[]), None);
        assert_eq!(sid_len(&[2, 0, 0, 0, 0, 0, 0, 5]), None);
        assert_eq!(sid_len(&user_sid(1000)[..27]), None);
        let mut too_many = vec![1, 16, 0, 0, 0, 0, 0, 5];
        too_many.resize(8 + 4 * 16, 0);
        assert_eq!(sid_len(&too_many), None);
    }

    #[test]
    fn sids_match_only_for_the_same_principal() {
        let owner = user_sid(1000);
        assert!(sids_match(&owner, &user_sid(1000)));
        assert!(!sids_match(&owner, &user_sid(1001)));
        assert!(!sids_match(&owner, &owner[..27]));
        assert!(!sids_match(&[], &owner));
    }

    /// Reads the listener's DACL back with GetSecurityInfo and checks that it has exactly one
    /// ACE, which allows the current user's SID.
    #[cfg(windows)]
    #[test]
    fn owner_only_dacl_has_one_ace_for_the_current_user() {
        use windows_sys::Win32::Foundation::{ERROR_SUCCESS, LocalFree};
        use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
        use windows_sys::Win32::Security::{
            ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
            GetAclInformation, PSECURITY_DESCRIPTOR,
        };
        use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

        let name = format!(r"\\.\pipe\mcao-dacl-test-{}", std::process::id());
        let server = PipeServer::create(&name).expect("create pipe");
        let user_sid = current_user_sid().expect("current user SID");

        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: the listener is live, and every out pointer is writable. The descriptor that
        // GetSecurityInfo allocates is freed below.
        let status = unsafe {
            GetSecurityInfo(
                server.listener.as_raw_handle(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, ERROR_SUCCESS, "GetSecurityInfo");

        let mut info = ACL_SIZE_INFORMATION {
            AceCount: 0,
            AclBytesInUse: 0,
            AclBytesFree: 0,
        };
        // SAFETY: `dacl` is the DACL from GetSecurityInfo, and `info` is the structure that
        // AclSizeInformation requests.
        let ok = unsafe {
            GetAclInformation(
                dacl,
                ptr::addr_of_mut!(info).cast::<c_void>(),
                mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        };
        assert_ne!(ok, FALSE, "GetAclInformation");
        assert_eq!(info.AceCount, 1, "exactly one ACE");

        let mut ace: *mut c_void = ptr::null_mut();
        // SAFETY: index 0 exists because the count is one.
        assert_ne!(unsafe { GetAce(dacl, 0, &mut ace) }, FALSE, "GetAce");
        // SAFETY: the only ACE was added by owner_only_acl as an ACCESS_ALLOWED_ACE.
        let allowed = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        assert_eq!(
            u32::from(allowed.Header.AceType),
            ACCESS_ALLOWED_ACE_TYPE,
            "allow ACE"
        );
        let ace_sid = ptr::addr_of!(allowed.SidStart).cast_mut().cast::<c_void>();
        // SAFETY: both pointers name SIDs that stay alive for the call.
        assert_ne!(
            unsafe { EqualSid(ace_sid, user_sid.as_ptr().cast_mut().cast()) },
            FALSE,
            "ACE SID is the current user"
        );

        // SAFETY: GetSecurityInfo allocated `descriptor` with LocalAlloc, and nothing else frees it.
        unsafe { LocalFree(descriptor) };
    }
}
