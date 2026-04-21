use super::{channel_invalid_input, channel_transport_error};
use crate::error::Result;
use crate::memory::allocator::InterprocessMemoryHandle;
use crate::types::ChannelId;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle};

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

#[derive(Debug)]
pub(super) struct TransportSender {
    pipe: File,
    peer_process: OwnedHandle,
}

impl TransportSender {
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
        let memory_handle = match handle {
            InterprocessMemoryHandle::CpuSharedWin32Handle(handle) => handle,
            InterprocessMemoryHandle::GpuOpaqueWin32Handle(_) => {
                return Err(channel_invalid_input("unexpected gpu handle for cpu ipc"));
            }
        };

        let duplicated = SYSCALLS
            .duplicate_handle_to_process(
                memory_handle.as_raw_handle(),
                self.peer_process.as_raw_handle(),
            )
            .map_err(|source| channel_transport_error("DuplicateHandle", source))?;
        let raw = duplicated.into_raw_handle();
        let raw_value = u64::try_from(raw as usize)
            .map_err(|_| channel_invalid_input("handle value overflow"))?;
        self.write_all(&raw_value.to_le_bytes())
    }
}

#[derive(Debug)]
pub(super) struct TransportListener {
    server: OwnedHandle,
}

impl TransportListener {
    pub(super) fn bind(address: &EndpointAddress) -> Result<Self> {
        let server = SYSCALLS
            .create_named_pipe(address.as_str())
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
        })
    }
}

#[derive(Debug)]
pub(super) struct TransportReceiver {
    pipe: File,
}

impl TransportReceiver {
    pub(super) fn connect(address: &EndpointAddress) -> Result<Self> {
        let client = SYSCALLS
            .open_named_pipe_client(address.as_str())
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

    pub(super) fn recv_cpu_handle(&mut self) -> Result<InterprocessMemoryHandle> {
        let mut raw_bytes = [0_u8; 8];
        self.read_exact(&mut raw_bytes)?;
        let raw_value = u64::from_le_bytes(raw_bytes);
        let raw_usize = usize::try_from(raw_value)
            .map_err(|_| channel_invalid_input("handle value overflow"))?;
        let raw = raw_usize as RawHandle;
        if raw.is_null() || raw as isize == -1 {
            return Err(channel_invalid_input("received invalid handle"));
        }

        let owned = unsafe { OwnedHandle::from_raw_handle(raw) };
        Ok(InterprocessMemoryHandle::from_cpu_shared_handle(owned))
    }
}

#[cfg_attr(not(test), allow(dead_code))]
trait Syscalls: Sync {
    fn create_named_pipe(&self, name: &str) -> std::io::Result<OwnedHandle>;
    fn open_named_pipe_client(&self, name: &str) -> std::io::Result<OwnedHandle>;
    fn connect_named_pipe(&self, handle: RawHandle) -> std::io::Result<()>;
    fn get_named_pipe_client_process_id(&self, handle: RawHandle) -> std::io::Result<u32>;
    fn open_process_duplicatable_handle(&self, process_id: u32) -> std::io::Result<OwnedHandle>;
    fn duplicate_handle_to_process(
        &self,
        source: RawHandle,
        target_process: RawHandle,
    ) -> std::io::Result<OwnedHandle>;
}

#[cfg(not(test))]
static SYSCALLS: RealSyscalls = RealSyscalls;

#[cfg(test)]
static SYSCALLS: tests::support::MockSyscalls = tests::support::MockSyscalls;

struct RealSyscalls;

impl RealSyscalls {
    fn into_null_terminated(value: &str) -> Vec<u16> {
        OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}

impl Syscalls for RealSyscalls {
    fn create_named_pipe(&self, name: &str) -> std::io::Result<OwnedHandle> {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_OUTBOUND,
        };
        use windows_sys::Win32::System::Pipes::{
            CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
        };

        let wide_name = Self::into_null_terminated(name);
        let raw = unsafe {
            CreateNamedPipeW(
                wide_name.as_ptr(),
                PIPE_ACCESS_OUTBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
        }
    }

    fn open_named_pipe_client(&self, name: &str) -> std::io::Result<OwnedHandle> {
        use windows_sys::Win32::Foundation::{GENERIC_READ, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, OPEN_EXISTING,
        };

        let wide_name = Self::into_null_terminated(name);
        let raw = unsafe {
            CreateFileW(
                wide_name.as_ptr(),
                GENERIC_READ,
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
    ) -> std::io::Result<OwnedHandle> {
        use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        // current_process is a pseudo-handle that doesn't need to be closed, so we don't wrap it in OwnedHandle.
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
            Ok(unsafe { OwnedHandle::from_raw_handle(duplicated) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::support::{
        BUFFER_SIZE, TestMeta, USED_SIZE, test_allocator, test_pair,
    };
    use super::*;
    use crate::{channels::MetadataEncoding, error::LavaFlowError};

    pub(in crate::channels::local) mod support {
        use super::*;

        thread_local! {
            static FAIL_OP_WINDOWS: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) };
        }

        pub(in crate::channels::local) fn set_fail(op: &'static str) {
            FAIL_OP_WINDOWS.with(|cell| {
                *cell.borrow_mut() = Some(op);
            });
        }

        fn should_fail(op: &'static str) -> bool {
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

        pub(in crate::channels::local) struct MockSyscalls;

        impl Syscalls for MockSyscalls {
            fn create_named_pipe(&self, name: &str) -> std::io::Result<OwnedHandle> {
                if should_fail("CreateNamedPipeW") {
                    Err(std::io::Error::other("CreateNamedPipeW failpoint"))
                } else {
                    RealSyscalls.create_named_pipe(name)
                }
            }

            fn open_named_pipe_client(&self, name: &str) -> std::io::Result<OwnedHandle> {
                if should_fail("CreateFileW") {
                    Err(std::io::Error::other("CreateFileW failpoint"))
                } else {
                    RealSyscalls.open_named_pipe_client(name)
                }
            }

            fn connect_named_pipe(&self, handle: RawHandle) -> std::io::Result<()> {
                if should_fail("ConnectNamedPipe") {
                    Err(std::io::Error::other("ConnectNamedPipe failpoint"))
                } else {
                    RealSyscalls.connect_named_pipe(handle)
                }
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
            ) -> std::io::Result<OwnedHandle> {
                if should_fail("DuplicateHandle") {
                    Err(std::io::Error::other("DuplicateHandle failpoint"))
                } else {
                    RealSyscalls.duplicate_handle_to_process(source, target_process)
                }
            }
        }
    }

    #[test]
    fn windows_handle_duplicate_failpoint_is_reported() {
        support::set_fail("DuplicateHandle");

        let (mut sender, _receiver) =
            test_pair(MetadataEncoding::Json).expect("create cpu local ipc pair");
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
}
