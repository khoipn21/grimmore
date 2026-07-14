use std::{
    ffi::{OsStr, OsString, c_void},
    io,
    mem::size_of,
    ptr::null_mut,
    slice,
};

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree},
        Security::Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        Security::{
            EqualSid, GetTokenInformation, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
        },
        System::{
            Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
            Threading::{
                GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    },
    core::PWSTR,
};

use crate::pipe_name::validate_local_named_pipe_name;

/// Builds the stable local endpoint name for the current Windows user without
/// exposing a machine-global pipe to another user's companion process.
pub fn current_user_pipe_endpoint(pipe_name: &str) -> io::Result<OsString> {
    crate::pipe_name::pipe_endpoint_for_sid(pipe_name, &current_user_sid_string()?)
}

/// Creates a local named-pipe instance whose protected DACL grants access only
/// to the current Windows user.
pub fn create_current_user_pipe(name: &OsStr, first_instance: bool) -> io::Result<NamedPipeServer> {
    validate_local_named_pipe_name(name)?;
    let mut security = CurrentUserSecurity::new()?;
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first_instance)
        .reject_remote_clients(true);

    // SAFETY: `security.attributes` is a valid `SECURITY_ATTRIBUTES` whose
    // descriptor allocation stays alive for the synchronous CreateNamedPipeW
    // call performed by Tokio. Windows copies the descriptor into the new
    // pipe object before this call returns.
    unsafe { options.create_with_security_attributes_raw(name, security.as_mut_ptr()) }
}

/// Returns whether the connected client process has the current user's SID.
pub fn named_pipe_client_is_current_user(pipe_handle: usize) -> io::Result<bool> {
    let process_id = named_pipe_process_id(pipe_handle, PipePeer::Client)?;
    process_is_current_user(process_id)
}

/// Returns whether the connected server process has the current user's SID.
pub fn named_pipe_server_is_current_user(pipe_handle: usize) -> io::Result<bool> {
    let process_id = named_pipe_process_id(pipe_handle, PipePeer::Server)?;
    process_is_current_user(process_id)
}

#[derive(Clone, Copy)]
enum PipePeer {
    Client,
    Server,
}

fn named_pipe_process_id(pipe_handle: usize, peer: PipePeer) -> io::Result<u32> {
    let mut process_id = 0;
    let handle = pipe_handle as HANDLE;
    // SAFETY: the caller obtains `pipe_handle` from a live Tokio named-pipe
    // object. The out pointer is valid for one `u32` and the API only reads
    // the opaque Windows handle.
    let result = unsafe {
        match peer {
            PipePeer::Client => GetNamedPipeClientProcessId(handle, &raw mut process_id),
            PipePeer::Server => GetNamedPipeServerProcessId(handle, &raw mut process_id),
        }
    };
    if result == 0 || process_id == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(process_id)
}

fn process_is_current_user(process_id: u32) -> io::Result<bool> {
    let current_user = TokenUserBuffer::for_process(current_process())?;
    let process = open_process_for_query(process_id)?;
    let peer_user = TokenUserBuffer::for_process(process.raw())?;
    // SAFETY: both SIDs are pointers supplied by successful
    // GetTokenInformation calls and remain valid while their backing token
    // buffers are alive for this comparison.
    Ok(unsafe { EqualSid(current_user.sid(), peer_user.sid()) != 0 })
}

fn current_user_sid_string() -> io::Result<String> {
    let user = TokenUserBuffer::for_process(current_process())?;
    let mut sid_string: PWSTR = null_mut();
    // SAFETY: `user.sid()` points to a token-owned SID while `user` is alive,
    // and `sid_string` is a valid out pointer. The API allocates its UTF-16
    // result with LocalAlloc for the RAII wrapper below.
    if unsafe { ConvertSidToStringSidW(user.sid(), &raw mut sid_string) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid_string = LocalAllocation::new(sid_string.cast());
    let length = wide_string_len(sid_string.as_wide())?;
    // SAFETY: ConvertSidToStringSidW returns a valid, null-terminated UTF-16
    // string. `sid_string` owns the allocation through this conversion.
    let units = unsafe { slice::from_raw_parts(sid_string.as_wide(), length) };
    let sid = String::from_utf16(units).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned an invalid UTF-16 SID",
        )
    })?;
    if !crate::pipe_name::is_windows_sid(&sid) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned an invalid SID string",
        ));
    }
    Ok(sid)
}

fn current_process() -> HANDLE {
    // SAFETY: GetCurrentProcess returns a pseudo-handle owned by the current
    // process and does not require CloseHandle.
    unsafe { GetCurrentProcess() }
}

fn open_process_for_query(process_id: u32) -> io::Result<OwnedHandle> {
    // SAFETY: OpenProcess accepts the requested read-only query right, no
    // inherited handle, and a process ID obtained from the connected pipe.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    OwnedHandle::new(handle)
}

struct CurrentUserSecurity {
    _descriptor: LocalAllocation,
    attributes: SECURITY_ATTRIBUTES,
}

impl CurrentUserSecurity {
    fn new() -> io::Result<Self> {
        let sid = current_user_sid_string()?;
        let descriptor = format!("D:P(A;;GA;;;{sid})");
        let descriptor_utf16 = descriptor
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut raw_descriptor = null_mut();
        // SAFETY: `descriptor_utf16` is a null-terminated SDDL string for the
        // duration of the call, and `raw_descriptor` is a valid out pointer.
        // The Windows API allocates the returned security descriptor with
        // LocalAlloc, which `LocalAllocation` releases.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                descriptor_utf16.as_ptr(),
                SDDL_REVISION_1,
                &raw mut raw_descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let descriptor = LocalAllocation::new(raw_descriptor);
        Ok(Self {
            attributes: SECURITY_ATTRIBUTES {
                nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
                    .expect("SECURITY_ATTRIBUTES fits in u32"),
                lpSecurityDescriptor: descriptor.raw(),
                bInheritHandle: 0,
            },
            _descriptor: descriptor,
        })
    }

    fn as_mut_ptr(&mut self) -> *mut c_void {
        (&raw mut self.attributes).cast()
    }
}

struct LocalAllocation(*mut c_void);

impl LocalAllocation {
    fn new(pointer: *mut c_void) -> Self {
        Self(pointer)
    }

    fn raw(&self) -> *mut c_void {
        self.0
    }

    fn as_wide(&self) -> *const u16 {
        self.0.cast()
    }
}

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this allocation was returned by a Windows API documented
            // to use LocalAlloc, and this wrapper has unique ownership.
            let _ = unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn new(handle: HANDLE) -> io::Result<Self> {
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(handle))
    }

    fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: OwnedHandle is created only after a successful OpenProcess
        // or OpenProcessToken call and is the sole closer of that handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct TokenUserBuffer {
    words: Vec<usize>,
}

impl TokenUserBuffer {
    fn for_process(process: HANDLE) -> io::Result<Self> {
        let mut token = null_mut();
        // SAFETY: `process` is either the current-process pseudo-handle or an
        // OwnedHandle from OpenProcess. `token` is a valid out pointer and the
        // requested right permits only token metadata reads.
        if unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle::new(token)?;
        let mut byte_count = 0;
        // SAFETY: the null information buffer is the documented sizing call;
        // `byte_count` is a valid out pointer.
        unsafe {
            GetTokenInformation(token.raw(), TokenUser, null_mut(), 0, &raw mut byte_count);
        }
        if byte_count < u32::try_from(size_of::<TOKEN_USER>()).expect("TOKEN_USER fits in u32") {
            return Err(io::Error::last_os_error());
        }
        let required = usize::try_from(byte_count).expect("u32 fits in usize");
        let word_count = required.div_ceil(size_of::<usize>());
        let mut words = vec![0_usize; word_count];
        // SAFETY: `words` has pointer alignment and at least `byte_count`
        // writable bytes. GetTokenInformation initializes a TOKEN_USER value
        // and the SID data entirely within that allocation.
        if unsafe {
            GetTokenInformation(
                token.raw(),
                TokenUser,
                words.as_mut_ptr().cast(),
                byte_count,
                &raw mut byte_count,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { words })
    }

    fn sid(&self) -> PSID {
        // SAFETY: `words` is aligned for TOKEN_USER and populated by the
        // successful GetTokenInformation call in `for_process`.
        unsafe { (*self.words.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

fn wide_string_len(value: *const u16) -> io::Result<usize> {
    if value.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows returned a null SID string",
        ));
    }
    let mut length = 0_usize;
    loop {
        // SAFETY: ConvertSidToStringSidW guarantees a null-terminated UTF-16
        // allocation. The pointer remains owned by LocalAllocation while this
        // bounded-by-terminator scan runs.
        if unsafe { *value.add(length) } == 0 {
            return Ok(length);
        }
        length = length.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Windows SID string is too long")
        })?;
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle as _;

    use tokio::net::windows::named_pipe::ClientOptions;

    use super::{
        create_current_user_pipe, current_user_pipe_endpoint, named_pipe_client_is_current_user,
        named_pipe_server_is_current_user,
    };
    use crate::validate_local_named_pipe_name;

    #[test]
    fn rejects_non_local_or_nested_pipe_names() {
        assert!(validate_local_named_pipe_name(r"\\.\pipe\grimmore-test".as_ref()).is_ok());
        assert!(validate_local_named_pipe_name(r"\\server\pipe\grimmore-test".as_ref()).is_err());
        assert!(validate_local_named_pipe_name(r"\\.\pipe\nested\name".as_ref()).is_err());
    }

    #[test]
    fn current_user_endpoint_is_stable_and_local() {
        let first = current_user_pipe_endpoint("grimmore-v1").expect("derive private endpoint");
        let second = current_user_pipe_endpoint("grimmore-v1").expect("derive private endpoint");

        assert_eq!(first, second);
        validate_local_named_pipe_name(&first).expect("validate private endpoint");
    }

    #[test]
    fn first_instance_rejects_a_preexisting_pipe() {
        let name = format!(
            r"\\.\pipe\grimmore-first-instance-test-{}",
            std::process::id()
        );
        let _squatter = create_current_user_pipe(name.as_ref(), false)
            .expect("create a preexisting current-user pipe");

        let Err(error) = create_current_user_pipe(name.as_ref(), true) else {
            panic!("first-instance pipe creation accepted a preexisting pipe");
        };
        assert_eq!(
            error.raw_os_error(),
            Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED.cast_signed()),
            "Windows must reserve the first-instance name against a pipe squatter"
        );
    }

    #[tokio::test]
    async fn current_user_pipe_checks_both_peer_process_sids() {
        let name = format!(r"\\.\pipe\grimmore-native-test-{}", std::process::id());
        let server = create_current_user_pipe(name.as_ref(), true).expect("create private pipe");
        let client = ClientOptions::new()
            .open(std::ffi::OsStr::new(&name))
            .expect("connect private pipe");
        server.connect().await.expect("accept private pipe client");

        assert!(
            named_pipe_client_is_current_user(server.as_raw_handle() as usize)
                .expect("inspect client SID")
        );
        assert!(
            named_pipe_server_is_current_user(client.as_raw_handle() as usize)
                .expect("inspect server SID")
        );
    }
}
