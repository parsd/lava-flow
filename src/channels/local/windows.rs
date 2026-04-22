use super::{FrameKind, channel_protocol_error, channel_transport_error};
use crate::error::Result;
use crate::memory::allocator::InterprocessMemoryHandle;
use crate::types::ChannelId;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::marker::PhantomPinned;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::pin::Pin;
#[cfg(test)]
use windows_sys::Win32::Security::WinWorldSid;
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAce, CopySid, CreateWellKnownSid,
    GetLengthSid, GetTokenInformation, InitializeAcl, InitializeSecurityDescriptor,
    SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES,
    SetSecurityDescriptorDacl, TOKEN_GROUPS, TOKEN_QUERY, TokenGroups,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_READ_EA, FILE_WRITE_DATA, READ_CONTROL, SYNCHRONIZE,
};
use windows_sys::Win32::System::SystemServices::SE_GROUP_LOGON_ID;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndpointAddress(String);

impl EndpointAddress {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_channel(channel_id: &ChannelId) -> Self {
        Self(format!(r"\\.\pipe\lava-flow-{}", channel_id.as_str()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl FrameKind {
    pub(super) fn from_handle(handle: &InterprocessMemoryHandle) -> Self {
        match handle {
            InterprocessMemoryHandle::GpuOpaqueWin32Handle(_) => Self::Gpu,
            InterprocessMemoryHandle::CpuSharedWin32Handle(_) => Self::Cpu,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PipeAccessMask(u32);

impl PipeAccessMask {
    fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
struct PipeAccessMaskBuilder {
    bits: u32,
}

impl PipeAccessMaskBuilder {
    fn new() -> Self {
        Self::default()
    }

    // Allow reading payload/control bytes from the named pipe.
    fn read_data(mut self) -> Self {
        self.bits |= FILE_READ_DATA;
        self
    }

    // Allow reading named-pipe metadata/attributes during CreateFileW and later handle use.
    fn read_attributes(mut self) -> Self {
        self.bits |= FILE_READ_ATTRIBUTES;
        self
    }

    // Allow reading extended attributes, which Windows also checks on this pipe-open path.
    fn read_extended_attributes(mut self) -> Self {
        self.bits |= FILE_READ_EA;
        self
    }

    // Allow writing protocol bytes such as receiver ACK/NACK messages on the duplex pipe.
    fn write_data(mut self) -> Self {
        self.bits |= FILE_WRITE_DATA;
        self
    }

    // Allow the access check to read the pipe object's security descriptor metadata.
    fn read_control(mut self) -> Self {
        self.bits |= READ_CONTROL;
        self
    }

    // Allow normal synchronous waiting and blocking I/O on the pipe handle.
    fn synchronize(mut self) -> Self {
        self.bits |= SYNCHRONIZE;
        self
    }

    fn build(self) -> PipeAccessMask {
        PipeAccessMask(self.bits)
    }
}

fn local_pipe_client_access_mask() -> PipeAccessMask {
    PipeAccessMaskBuilder::new()
        // Required for reading protocol bytes and imported-handle payload markers from the duplex pipe.
        .read_data()
        // Required for the client open path; Windows checks pipe attributes during duplex CreateFileW.
        .read_attributes()
        // Required for the client open path; without EA-read permission, the duplex CreateFileW is denied.
        .read_extended_attributes()
        // Required for sending import ACK/NACK bytes back to the sender on the same duplex pipe.
        .write_data()
        // Required so the client can pass the pipe-object security descriptor access check.
        .read_control()
        // Required for normal synchronous blocking I/O on the named-pipe handle.
        .synchronize()
        .build()
}

struct NamedPipeSecurityDescriptor {
    // SECURITY_ATTRIBUTES is the small wrapper passed into CreateNamedPipeW. It carries a pointer
    // to the real security descriptor and also controls whether the returned handle is inheritable.
    security_attributes: SECURITY_ATTRIBUTES,
    // SECURITY_DESCRIPTOR is the Windows object that owns the DACL definition for the pipe.
    security_descriptor: SECURITY_DESCRIPTOR,
    // The ACL memory must stay alive as long as the security descriptor points at it, so we keep
    // the raw storage here alongside the descriptor wrapper.
    _acl_words: Vec<u32>,
    // Pinning matters because SECURITY_ATTRIBUTES stores a raw pointer to security_descriptor.
    _pin: PhantomPinned,
}

impl NamedPipeSecurityDescriptor {
    fn current_logon_session(access_mask: PipeAccessMask) -> std::io::Result<Pin<Box<Self>>> {
        Self::from_sid(access_mask, SYSCALLS.current_logon_session_sid_words()?)
    }

    #[cfg(test)]
    fn everyone(access_mask: PipeAccessMask) -> std::io::Result<Pin<Box<Self>>> {
        Self::from_sid(access_mask, SYSCALLS.well_known_sid_words(WinWorldSid)?)
    }

    fn from_sid(
        access_mask: PipeAccessMask,
        sid_words: Vec<u32>,
    ) -> std::io::Result<Pin<Box<Self>>> {
        // A SID (security identifier) is Windows' variable-length binary identity for a user,
        // group, or logon session. The ACL stores SID bytes in each ACE to describe who is
        // allowed to access the named pipe.
        let sid_size = SYSCALLS.sid_size(&sid_words);
        let acl_size =
            std::mem::size_of::<ACL>() + std::mem::size_of::<ACCESS_ALLOWED_ACE>() + sid_size
                - std::mem::size_of::<u32>();
        let mut acl_words = vec![0_u32; acl_size.div_ceil(std::mem::size_of::<u32>())];
        let acl = acl_words.as_mut_ptr().cast::<ACL>();
        SYSCALLS.initialize_acl(acl, acl_size as u32)?;
        SYSCALLS.add_access_allowed_ace(acl, access_mask.bits(), &sid_words)?;

        let mut security_descriptor = unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() };
        SYSCALLS.initialize_security_descriptor(
            (&mut security_descriptor as *mut SECURITY_DESCRIPTOR).cast(),
        )?;
        SYSCALLS.set_security_descriptor_dacl(
            (&mut security_descriptor as *mut SECURITY_DESCRIPTOR).cast(),
            acl,
        )?;

        let security_attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 0,
        };

        let mut descriptor = Box::pin(Self {
            security_attributes,
            security_descriptor,
            _acl_words: acl_words,
            _pin: PhantomPinned,
        });
        unsafe {
            let this = descriptor.as_mut().get_unchecked_mut();
            this.security_attributes.lpSecurityDescriptor =
                (&mut this.security_descriptor as *mut SECURITY_DESCRIPTOR).cast();
        }
        Ok(descriptor)
    }

    fn as_mut_ptr(self: Pin<&mut Self>) -> *mut SECURITY_ATTRIBUTES {
        let this = unsafe { self.get_unchecked_mut() };
        &mut this.security_attributes
    }
}

#[derive(Debug)]
pub(super) struct TransportSender {
    pipe: File,
    peer_process: OwnedHandle,
    pending_remote_handle: Option<RawHandle>,
}

impl TransportSender {
    pub(super) fn read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.pipe
            .read_exact(bytes)
            .map_err(|source| channel_transport_error("read_exact", source))
    }

    pub(super) fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.pipe
            .write_all(bytes)
            .map_err(|source| channel_transport_error("write_all", source))
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        self.pipe
            .flush()
            .map_err(|source| channel_transport_error("flush", source))
    }

    pub(super) fn send_cpu_handle(&mut self, handle: InterprocessMemoryHandle) -> Result<()> {
        if self.pending_remote_handle.is_some() {
            return Err(channel_protocol_error(
                "send_cpu_handle",
                "overlapping pending cpu handle transfer",
            ));
        }

        let memory_handle = match handle {
            InterprocessMemoryHandle::CpuSharedWin32Handle(handle) => handle,
            InterprocessMemoryHandle::GpuOpaqueWin32Handle(_) => {
                return Err(channel_protocol_error(
                    "send_cpu_handle",
                    "unexpected gpu handle for cpu ipc",
                ));
            }
        };

        let duplicated = SYSCALLS
            .duplicate_handle_to_process(
                memory_handle.as_raw_handle(),
                self.peer_process.as_raw_handle(),
            )
            .map_err(|source| channel_transport_error("DuplicateHandle", source))?;
        let raw_value = duplicated as usize as u64;
        #[cfg(test)]
        if tests::support::should_fail("write_raw_handle_value") {
            let _ = self.close_remote_handle(duplicated);
            return Err(channel_transport_error(
                "write_raw_handle_value",
                std::io::Error::other("write_raw_handle_value failpoint"),
            ));
        }

        match self.write_all(&raw_value.to_le_bytes()) {
            Ok(()) => {
                self.pending_remote_handle = Some(duplicated);
                Ok(())
            }
            Err(error) => {
                let _ = self.close_remote_handle(duplicated);
                Err(error)
            }
        }
    }

    pub(super) fn complete_transfer(&mut self) {
        self.pending_remote_handle = None;
    }

    pub(super) fn abort_transfer(&mut self) {
        if let Some(remote_handle) = self.pending_remote_handle.take() {
            let _ = self.close_remote_handle(remote_handle);
        }
    }

    fn close_remote_handle(&self, remote_handle: RawHandle) -> std::io::Result<()> {
        SYSCALLS.close_remote_handle_in_process(self.peer_process.as_raw_handle(), remote_handle)
    }
}

#[derive(Debug)]
pub(super) struct TransportListener {
    server: OwnedHandle,
}

impl TransportListener {
    pub(super) fn bind(address: &EndpointAddress) -> Result<Self> {
        let mut security =
            NamedPipeSecurityDescriptor::current_logon_session(local_pipe_client_access_mask())
                .map_err(|source| channel_transport_error("build_named_pipe_security", source))?;
        let server = SYSCALLS
            .create_named_pipe(address.as_str(), Some(security.as_mut()))
            .map_err(|source| channel_transport_error("CreateNamedPipeW", source))?;
        Ok(Self { server })
    }

    pub(super) fn accept(self) -> Result<TransportSender> {
        SYSCALLS
            .connect_named_pipe(self.server.as_raw_handle())
            .map_err(|source| channel_transport_error("ConnectNamedPipe", source))?;
        let peer_process_id = SYSCALLS
            .get_named_pipe_client_process_id(self.server.as_raw_handle())
            .map_err(|source| channel_transport_error("GetNamedPipeClientProcessId", source))?;
        let peer_process = SYSCALLS
            .open_process_duplicatable_handle(peer_process_id)
            .map_err(|source| channel_transport_error("OpenProcess", source))?;
        Ok(TransportSender {
            pipe: File::from(self.server),
            peer_process,
            pending_remote_handle: None,
        })
    }
}

#[derive(Debug)]
pub(super) struct TransportReceiver {
    pipe: File,
}

impl TransportReceiver {
    pub(super) fn connect(address: &EndpointAddress) -> Result<Self> {
        let access_mask = local_pipe_client_access_mask();
        let client = SYSCALLS
            .open_named_pipe_client(address.as_str(), access_mask)
            .map_err(|source| channel_transport_error("CreateFileW", source))?;
        Ok(Self {
            pipe: File::from(client),
        })
    }

    pub(super) fn read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.pipe
            .read_exact(bytes)
            .map_err(|source| channel_transport_error("read_exact", source))
    }

    pub(super) fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.pipe
            .write_all(bytes)
            .map_err(|source| channel_transport_error("write_all", source))
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        self.pipe
            .flush()
            .map_err(|source| channel_transport_error("flush", source))
    }

    pub(super) fn recv_cpu_handle(&mut self) -> Result<InterprocessMemoryHandle> {
        let mut raw_bytes = [0_u8; 8];
        self.read_exact(&mut raw_bytes)?;
        let raw_value = u64::from_le_bytes(raw_bytes);
        let raw_usize = usize::try_from(raw_value)
            .map_err(|_| channel_protocol_error("recv_cpu_handle", "handle value overflow"))?;
        let raw = raw_usize as RawHandle;
        if raw.is_null() || raw as isize == -1 {
            return Err(channel_protocol_error(
                "recv_cpu_handle",
                "received invalid handle",
            ));
        }

        let owned = unsafe { OwnedHandle::from_raw_handle(raw) };
        Ok(InterprocessMemoryHandle::from_cpu_shared_handle(owned))
    }
}

#[cfg_attr(not(test), allow(dead_code))]
trait Syscalls: Sync {
    fn create_named_pipe(
        &self,
        name: &str,
        security: Option<Pin<&mut NamedPipeSecurityDescriptor>>,
    ) -> std::io::Result<OwnedHandle>;
    fn open_named_pipe_client(
        &self,
        name: &str,
        access_mask: PipeAccessMask,
    ) -> std::io::Result<OwnedHandle>;
    fn connect_named_pipe(&self, handle: RawHandle) -> std::io::Result<()>;
    fn sid_size(&self, sid_words: &[u32]) -> usize;
    fn current_logon_session_sid_words(&self) -> std::io::Result<Vec<u32>>;
    fn well_known_sid_words(&self, sid_type: i32) -> std::io::Result<Vec<u32>>;
    fn initialize_acl(&self, acl: *mut ACL, acl_size: u32) -> std::io::Result<()>;
    fn add_access_allowed_ace(
        &self,
        acl: *mut ACL,
        access_mask: u32,
        sid_words: &[u32],
    ) -> std::io::Result<()>;
    fn initialize_security_descriptor(
        &self,
        security_descriptor: *mut core::ffi::c_void,
    ) -> std::io::Result<()>;
    fn set_security_descriptor_dacl(
        &self,
        security_descriptor: *mut core::ffi::c_void,
        acl: *mut ACL,
    ) -> std::io::Result<()>;
    fn get_named_pipe_client_process_id(&self, handle: RawHandle) -> std::io::Result<u32>;
    fn open_process_duplicatable_handle(&self, process_id: u32) -> std::io::Result<OwnedHandle>;
    fn duplicate_handle_to_process(
        &self,
        source: RawHandle,
        target_process: RawHandle,
    ) -> std::io::Result<RawHandle>;
    fn close_remote_handle_in_process(
        &self,
        source_process: RawHandle,
        source: RawHandle,
    ) -> std::io::Result<()>;
}

#[cfg(not(test))]
static SYSCALLS: RealSyscalls = RealSyscalls;

#[cfg(test)]
static SYSCALLS: tests::support::MockSyscalls = tests::support::MockSyscalls;

struct RealSyscalls;

impl RealSyscalls {
    const SECURITY_DESCRIPTOR_REVISION_1: u32 = 1;

    fn into_null_terminated(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn create_named_pipe_impl(
        &self,
        name: &str,
        mut security: Option<Pin<&mut NamedPipeSecurityDescriptor>>,
    ) -> std::io::Result<OwnedHandle> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX,
        };
        use windows_sys::Win32::System::Pipes::{
            CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
        };

        let wide_name = Self::into_null_terminated(name);
        let raw = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                4096,
                4096,
                0,
                security
                    .as_mut()
                    .map(|value| value.as_mut().as_mut_ptr())
                    .unwrap_or(std::ptr::null_mut()),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
        }
    }

    fn open_named_pipe_client_impl(
        &self,
        name: &str,
        access_mask: PipeAccessMask,
    ) -> std::io::Result<OwnedHandle> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
        };

        let wide_name = Self::into_null_terminated(name);
        let raw = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                access_mask.bits(),
                0,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
        }
    }

    fn open_current_process_token_query(&self) -> std::io::Result<OwnedHandle> {
        let process = unsafe { GetCurrentProcess() };
        let mut token = std::ptr::null_mut();
        let token_opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
        if token_opened == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedHandle::from_raw_handle(token) })
        }
    }

    fn token_groups_bytes(&self, token: RawHandle) -> std::io::Result<Vec<u8>> {
        let mut required = 0_u32;
        unsafe { GetTokenInformation(token, TokenGroups, std::ptr::null_mut(), 0, &mut required) };

        let mut buffer = vec![0_u8; required as usize];
        let groups_read = unsafe {
            GetTokenInformation(
                token,
                TokenGroups,
                buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        };
        if groups_read == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(buffer)
        }
    }

    fn find_logon_group_sid(
        &self,
        token_groups_bytes: &[u8],
    ) -> std::io::Result<*mut core::ffi::c_void> {
        let groups = token_groups_bytes.as_ptr().cast::<TOKEN_GROUPS>();
        let count = unsafe { (*groups).GroupCount as usize };
        let first = unsafe { std::ptr::addr_of!((*groups).Groups[0]) };
        let groups_slice = unsafe { std::slice::from_raw_parts(first, count) };
        let logon_group = groups_slice
            .iter()
            .find(|group: &&SID_AND_ATTRIBUTES| {
                (group.Attributes & SE_GROUP_LOGON_ID as u32) == SE_GROUP_LOGON_ID as u32
            })
            .ok_or_else(|| std::io::Error::other("logon sid not found in token groups"))?;
        Ok(logon_group.Sid)
    }

    fn copy_sid_words_from_ptr(&self, sid: *mut core::ffi::c_void) -> std::io::Result<Vec<u32>> {
        let sid_size = unsafe { GetLengthSid(sid) } as usize;
        let mut sid_words = vec![0_u32; sid_size.div_ceil(std::mem::size_of::<u32>())];
        let sid_copied = unsafe { CopySid(sid_size as u32, sid_words.as_mut_ptr().cast(), sid) };
        if sid_copied == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(sid_words)
        }
    }
}

impl Syscalls for RealSyscalls {
    fn create_named_pipe(
        &self,
        name: &str,
        security: Option<Pin<&mut NamedPipeSecurityDescriptor>>,
    ) -> std::io::Result<OwnedHandle> {
        RealSyscalls::create_named_pipe_impl(self, name, security)
    }

    fn open_named_pipe_client(
        &self,
        name: &str,
        access_mask: PipeAccessMask,
    ) -> std::io::Result<OwnedHandle> {
        RealSyscalls::open_named_pipe_client_impl(self, name, access_mask)
    }

    fn connect_named_pipe(&self, handle: RawHandle) -> std::io::Result<()> {
        use windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED;
        use windows_sys::Win32::System::Pipes::ConnectNamedPipe;

        let ok = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) };
        if ok != 0 {
            return Ok(());
        }

        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32) {
            Ok(())
        } else {
            Err(err)
        }
    }

    fn sid_size(&self, sid_words: &[u32]) -> usize {
        unsafe { GetLengthSid(sid_words.as_ptr().cast_mut().cast()) as usize }
    }

    fn current_logon_session_sid_words(&self) -> std::io::Result<Vec<u32>> {
        let token = self.open_current_process_token_query()?;
        let token_groups_bytes = self.token_groups_bytes(token.as_raw_handle())?;
        let logon_sid = self.find_logon_group_sid(&token_groups_bytes)?;
        self.copy_sid_words_from_ptr(logon_sid)
    }

    fn well_known_sid_words(&self, sid_type: i32) -> std::io::Result<Vec<u32>> {
        let mut sid_words =
            vec![0_u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(std::mem::size_of::<u32>())];
        let mut sid_size = (sid_words.len() * std::mem::size_of::<u32>()) as u32;
        let sid_created = unsafe {
            CreateWellKnownSid(
                sid_type,
                std::ptr::null_mut(),
                sid_words.as_mut_ptr().cast(),
                &mut sid_size,
            )
        };
        if sid_created == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(sid_words)
    }

    fn initialize_acl(&self, acl: *mut ACL, acl_size: u32) -> std::io::Result<()> {
        let ok = unsafe { InitializeAcl(acl, acl_size, ACL_REVISION) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn add_access_allowed_ace(
        &self,
        acl: *mut ACL,
        access_mask: u32,
        sid_words: &[u32],
    ) -> std::io::Result<()> {
        let ok = unsafe {
            AddAccessAllowedAce(
                acl,
                ACL_REVISION,
                access_mask,
                sid_words.as_ptr().cast_mut().cast(),
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn initialize_security_descriptor(
        &self,
        security_descriptor: *mut core::ffi::c_void,
    ) -> std::io::Result<()> {
        let ok = unsafe {
            InitializeSecurityDescriptor(
                security_descriptor,
                RealSyscalls::SECURITY_DESCRIPTOR_REVISION_1,
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn set_security_descriptor_dacl(
        &self,
        security_descriptor: *mut core::ffi::c_void,
        acl: *mut ACL,
    ) -> std::io::Result<()> {
        let ok = unsafe { SetSecurityDescriptorDacl(security_descriptor, 1, acl, 0) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn get_named_pipe_client_process_id(&self, handle: RawHandle) -> std::io::Result<u32> {
        use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;

        let mut process_id = 0_u32;
        let ok = unsafe { GetNamedPipeClientProcessId(handle, &mut process_id) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(process_id)
        }
    }

    fn open_process_duplicatable_handle(&self, process_id: u32) -> std::io::Result<OwnedHandle> {
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_DUP_HANDLE};

        let raw = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, process_id) };
        if raw.is_null() {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
        }
    }

    fn duplicate_handle_to_process(
        &self,
        source: RawHandle,
        target_process: RawHandle,
    ) -> std::io::Result<RawHandle> {
        use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        // current_process is a pseudo-handle that doesn't need to be closed, so we don't wrap it
        // in OwnedHandle. The returned duplicated value is valid in the target process, not this
        // one, so it must stay an opaque raw handle here.
        let current_process = unsafe { GetCurrentProcess() };
        let mut duplicated = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                current_process,
                source,
                target_process,
                &mut duplicated,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(duplicated)
        }
    }

    fn close_remote_handle_in_process(
        &self,
        source_process: RawHandle,
        source: RawHandle,
    ) -> std::io::Result<()> {
        use windows_sys::Win32::Foundation::{
            DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let current_process = unsafe { GetCurrentProcess() };
        let mut duplicated = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                source_process,
                source,
                current_process,
                &mut duplicated,
                0,
                0,
                DUPLICATE_SAME_ACCESS | DUPLICATE_CLOSE_SOURCE,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }

        if duplicated.is_null() || duplicated as isize == -1 {
            return Err(std::io::Error::other(
                "DuplicateHandle returned invalid local handle during close-source rollback",
            ));
        }

        let duplicated = unsafe { OwnedHandle::from_raw_handle(duplicated) };
        drop(duplicated);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::support::{
        BUFFER_SIZE, TestMeta, USED_SIZE, test_allocator, test_pair,
    };
    use super::*;
    use crate::{channels::MetadataEncoding, error::LavaFlowError};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    pub(in crate::channels::local) mod support {
        use super::*;

        thread_local! {
            static FAIL_OP_WINDOWS: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) };
            static REMOTE_CLOSE_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
        }

        pub(in crate::channels::local) fn set_fail(op: &'static str) {
            FAIL_OP_WINDOWS.with(|cell| {
                *cell.borrow_mut() = Some(op);
            });
        }

        pub(in crate::channels::local) fn should_fail(op: &'static str) -> bool {
            FAIL_OP_WINDOWS.with(|cell| {
                let mut current = cell.borrow_mut();
                if current.as_ref() == Some(&op) {
                    *current = None;
                    true
                } else {
                    false
                }
            })
        }

        pub(in crate::channels::local) fn reset_remote_close_calls() {
            REMOTE_CLOSE_CALLS.with(|cell| cell.set(0));
        }

        pub(in crate::channels::local) fn remote_close_calls() -> u32 {
            REMOTE_CLOSE_CALLS.with(|cell| cell.get())
        }

        pub(in crate::channels::local) struct MockSyscalls;

        impl Syscalls for MockSyscalls {
            fn create_named_pipe(
                &self,
                name: &str,
                security: Option<Pin<&mut NamedPipeSecurityDescriptor>>,
            ) -> std::io::Result<OwnedHandle> {
                if should_fail("CreateNamedPipeW") {
                    Err(std::io::Error::other("CreateNamedPipeW failpoint"))
                } else {
                    RealSyscalls.create_named_pipe(name, security)
                }
            }

            fn open_named_pipe_client(
                &self,
                name: &str,
                access_mask: PipeAccessMask,
            ) -> std::io::Result<OwnedHandle> {
                if should_fail("CreateFileW") {
                    Err(std::io::Error::other("CreateFileW failpoint"))
                } else {
                    RealSyscalls.open_named_pipe_client(name, access_mask)
                }
            }

            fn connect_named_pipe(&self, handle: RawHandle) -> std::io::Result<()> {
                if should_fail("ConnectNamedPipe") {
                    Err(std::io::Error::other("ConnectNamedPipe failpoint"))
                } else {
                    RealSyscalls.connect_named_pipe(handle)
                }
            }

            fn sid_size(&self, sid_words: &[u32]) -> usize {
                RealSyscalls.sid_size(sid_words)
            }

            fn current_logon_session_sid_words(&self) -> std::io::Result<Vec<u32>> {
                RealSyscalls.current_logon_session_sid_words()
            }

            fn well_known_sid_words(&self, sid_type: i32) -> std::io::Result<Vec<u32>> {
                RealSyscalls.well_known_sid_words(sid_type)
            }

            fn initialize_acl(&self, acl: *mut ACL, acl_size: u32) -> std::io::Result<()> {
                RealSyscalls.initialize_acl(acl, acl_size)
            }

            fn add_access_allowed_ace(
                &self,
                acl: *mut ACL,
                access_mask: u32,
                sid_words: &[u32],
            ) -> std::io::Result<()> {
                RealSyscalls.add_access_allowed_ace(acl, access_mask, sid_words)
            }

            fn initialize_security_descriptor(
                &self,
                security_descriptor: *mut core::ffi::c_void,
            ) -> std::io::Result<()> {
                RealSyscalls.initialize_security_descriptor(security_descriptor)
            }

            fn set_security_descriptor_dacl(
                &self,
                security_descriptor: *mut core::ffi::c_void,
                acl: *mut ACL,
            ) -> std::io::Result<()> {
                RealSyscalls.set_security_descriptor_dacl(security_descriptor, acl)
            }

            fn get_named_pipe_client_process_id(&self, handle: RawHandle) -> std::io::Result<u32> {
                if should_fail("GetNamedPipeClientProcessId") {
                    Err(std::io::Error::other(
                        "GetNamedPipeClientProcessId failpoint",
                    ))
                } else {
                    RealSyscalls.get_named_pipe_client_process_id(handle)
                }
            }

            fn open_process_duplicatable_handle(
                &self,
                process_id: u32,
            ) -> std::io::Result<OwnedHandle> {
                if should_fail("OpenProcess") {
                    Err(std::io::Error::other("OpenProcess failpoint"))
                } else {
                    RealSyscalls.open_process_duplicatable_handle(process_id)
                }
            }

            fn duplicate_handle_to_process(
                &self,
                source: RawHandle,
                target_process: RawHandle,
            ) -> std::io::Result<RawHandle> {
                if should_fail("DuplicateHandle") {
                    Err(std::io::Error::other("DuplicateHandle failpoint"))
                } else {
                    RealSyscalls.duplicate_handle_to_process(source, target_process)
                }
            }

            fn close_remote_handle_in_process(
                &self,
                source_process: RawHandle,
                source: RawHandle,
            ) -> std::io::Result<()> {
                REMOTE_CLOSE_CALLS.with(|cell| cell.set(cell.get() + 1));
                if should_fail("DuplicateHandleCloseSource") {
                    Err(std::io::Error::other(
                        "DuplicateHandleCloseSource failpoint",
                    ))
                } else {
                    RealSyscalls.close_remote_handle_in_process(source_process, source)
                }
            }
        }
    }

    #[test]
    fn windows_handle_duplicate_failpoint_is_reported() {
        let (mut sender, _receiver) =
            test_pair(MetadataEncoding::Json).expect("create cpu local ipc pair");
        support::set_fail("DuplicateHandle");
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let metadata = TestMeta {
            used_size: USED_SIZE,
            width: 32,
            height: 32,
        };

        let err = sender
            .send(buffer, &metadata)
            .expect_err("duplicate failpoint must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "DuplicateHandle",
                ..
            }
        ));
    }

    #[test]
    fn windows_remote_handle_is_rolled_back_when_handle_value_write_fails() {
        let (mut sender, _receiver) =
            test_pair(MetadataEncoding::Json).expect("create cpu local ipc pair");
        support::reset_remote_close_calls();
        support::set_fail("write_raw_handle_value");
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let metadata = TestMeta {
            used_size: USED_SIZE,
            width: 32,
            height: 32,
        };

        let err = sender
            .send(buffer, &metadata)
            .expect_err("write failpoint must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "write_raw_handle_value",
                ..
            }
        ));
        assert_eq!(
            support::remote_close_calls(),
            1,
            "sender must best-effort close the duplicated remote handle on send failure",
        );
    }

    #[test]
    fn allow_everyone_builder_allows_client_open() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let channel_id = ChannelId::new(format!("allow-everyone-{id}")).expect("channel id");
        let address = EndpointAddress::from_channel(&channel_id);

        let mut security = NamedPipeSecurityDescriptor::everyone(local_pipe_client_access_mask())
            .expect("build everyone security descriptor");

        let server = SYSCALLS
            .create_named_pipe(address.as_str(), Some(security.as_mut()))
            .expect("create named pipe with everyone dacl");
        let server_handle = server.as_raw_handle() as usize;
        let connect_thread = thread::spawn(move || {
            SYSCALLS
                .connect_named_pipe(server_handle as RawHandle)
                .expect("connect named pipe");
        });

        let client = SYSCALLS
            .open_named_pipe_client(address.as_str(), local_pipe_client_access_mask())
            .expect("open named pipe client with everyone dacl");

        connect_thread
            .join()
            .expect("connect thread must not panic");
        drop(client);
    }
}
