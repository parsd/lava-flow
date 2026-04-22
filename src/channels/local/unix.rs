use super::{FrameKind, channel_protocol_error, channel_transport_error};
use crate::error::Result;
use crate::memory::allocator::InterprocessMemoryHandle;
use crate::types::ChannelId;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndpointAddress(String);

impl EndpointAddress {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_channel(channel_id: &ChannelId) -> Self {
        let path = runtime_dir_path().join(format!("{}.sock", channel_id.as_str()));
        Self(path.to_string_lossy().into_owned())
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl FrameKind {
    pub(super) fn from_handle(handle: &InterprocessMemoryHandle) -> Self {
        match handle {
            InterprocessMemoryHandle::GpuOpaqueFd(_) => Self::Gpu,
            InterprocessMemoryHandle::CpuSharedFd(_) => Self::Cpu,
        }
    }
}

#[derive(Debug)]
pub(super) struct TransportSender {
    stream: UnixStream,
}

impl TransportSender {
    pub(super) fn read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.stream
            .read_exact(bytes)
            .map_err(|source| channel_transport_error("read_exact", source))
    }

    pub(super) fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream
            .write_all(bytes)
            .map_err(|source| channel_transport_error("write_all", source))
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        self.stream
            .flush()
            .map_err(|source| channel_transport_error("flush", source))
    }

    pub(super) fn send_cpu_handle(&mut self, handle: InterprocessMemoryHandle) -> Result<()> {
        let fd = match handle {
            InterprocessMemoryHandle::CpuSharedFd(fd) => fd,
            InterprocessMemoryHandle::GpuOpaqueFd(_) => {
                return Err(channel_protocol_error(
                    "send_cpu_handle",
                    "unexpected gpu handle for cpu ipc",
                ));
            }
        };

        self.send_fd(&fd)
    }

    pub(super) fn complete_transfer(&mut self) {}

    pub(super) fn abort_transfer(&mut self) {}

    fn send_fd(&self, fd: &OwnedFd) -> Result<()> {
        let mut marker = [0_u8; 1];
        let mut iov = libc::iovec {
            iov_base: marker.as_mut_ptr().cast(),
            iov_len: marker.len(),
        };
        let mut control = vec![
            0_u8;
            unsafe {
                libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as libc::c_uint) as usize
            }
        ];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        // msg_control points at the ancillary data buffer that carries the passed descriptor.
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = control.len();

        unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            if cmsg.is_null() {
                return Err(channel_protocol_error("send_fd", "missing cmsg header"));
            }
            // SCM_RIGHTS tells the kernel to duplicate this descriptor for the receiving process.
            (*cmsg).cmsg_level = libc::SOL_SOCKET;
            (*cmsg).cmsg_type = libc::SCM_RIGHTS;
            // The cmsghdr length covers the header plus exactly one passed file descriptor.
            (*cmsg).cmsg_len =
                libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as libc::c_uint) as usize;
            // CMSG_DATA is the payload area where the descriptor number itself is written.
            *(libc::CMSG_DATA(cmsg).cast::<libc::c_int>()) = fd.as_raw_fd();
        }

        SYSCALLS
            .sendmsg(self.stream.as_raw_fd(), &msg, 0)
            .map(|_| ())
            .map_err(|source| channel_transport_error("sendmsg", source))
    }
}

#[derive(Debug)]
pub(super) struct TransportListener {
    listener: UnixListener,
    path: PathBuf,
}

impl TransportListener {
    pub(super) fn bind(address: &EndpointAddress) -> Result<Self> {
        Self::ensure_runtime_dir_exists()?;
        let path = PathBuf::from(address.as_str());
        if let Err(source) = fs::remove_file(&path)
            && source.kind() != std::io::ErrorKind::NotFound
        {
            return Err(channel_transport_error("remove_file", source));
        }
        let listener =
            UnixListener::bind(&path).map_err(|source| channel_transport_error("bind", source))?;
        Ok(Self { listener, path })
    }

    fn ensure_runtime_dir_exists() -> Result<()> {
        let path = runtime_dir_path();
        fs::create_dir_all(&path)
            .map_err(|source| channel_transport_error("create_dir_all", source))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|source| channel_transport_error("set_permissions", source))
    }

    pub(super) fn accept(self) -> Result<TransportSender> {
        let (stream, _) = self
            .listener
            .accept()
            .map_err(|source| channel_transport_error("accept", source))?;
        Ok(TransportSender { stream })
    }
}

impl Drop for TransportListener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
pub(super) struct TransportReceiver {
    stream: UnixStream,
}

impl TransportReceiver {
    pub(super) fn connect(address: &EndpointAddress) -> Result<Self> {
        let stream = UnixStream::connect(address.as_str())
            .map_err(|source| channel_transport_error("connect", source))?;
        Ok(Self { stream })
    }

    pub(super) fn read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        self.stream
            .read_exact(bytes)
            .map_err(|source| channel_transport_error("read_exact", source))
    }

    pub(super) fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream
            .write_all(bytes)
            .map_err(|source| channel_transport_error("write_all", source))
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        self.stream
            .flush()
            .map_err(|source| channel_transport_error("flush", source))
    }

    pub(super) fn recv_cpu_handle(&mut self) -> Result<InterprocessMemoryHandle> {
        self.recv_fd()
            .map(InterprocessMemoryHandle::from_cpu_shared_fd)
    }

    fn recv_fd(&mut self) -> Result<OwnedFd> {
        let mut marker = [0_u8; 1];
        let mut iov = libc::iovec {
            iov_base: marker.as_mut_ptr().cast(),
            iov_len: marker.len(),
        };
        let mut control = vec![
            0_u8;
            unsafe {
                libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as libc::c_uint) as usize
            }
        ];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = control.len();

        SYSCALLS
            .recvmsg(self.stream.as_raw_fd(), &mut msg, 0)
            .map_err(|source| channel_transport_error("recvmsg", source))?;

        let fd = unsafe {
            let cmsg = libc::CMSG_FIRSTHDR(&msg);
            if cmsg.is_null() {
                return Err(channel_protocol_error("recv_fd", "missing received handle"));
            }
            if (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
                return Err(channel_protocol_error(
                    "recv_fd",
                    "unexpected received cmsg type",
                ));
            }
            *(libc::CMSG_DATA(cmsg).cast::<libc::c_int>())
        };

        SYSCALLS
            .fcntl_setfd_cloexec(fd)
            .map_err(|source| channel_transport_error("fcntl_cloexec", source))?;

        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(owned)
    }
}

fn runtime_dir_path() -> PathBuf {
    // Prefer the standard per-user runtime directory for local IPC sockets. It is typically
    // private to the current user and avoids exposing endpoints directly under /tmp.
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let uid = unsafe { libc::geteuid() };
            env::temp_dir().join(format!("lava-flow-uid-{uid}"))
        });
    base.join("lava-flow")
}

#[cfg_attr(not(test), allow(dead_code))]
trait Syscalls: Sync {
    fn sendmsg(
        &self,
        socket: libc::c_int,
        msg: &libc::msghdr,
        flags: libc::c_int,
    ) -> std::io::Result<usize>;
    fn recvmsg(
        &self,
        socket: libc::c_int,
        msg: &mut libc::msghdr,
        flags: libc::c_int,
    ) -> std::io::Result<usize>;
    fn fcntl_setfd_cloexec(&self, fd: libc::c_int) -> std::io::Result<()>;
}

#[cfg(not(test))]
static SYSCALLS: RealSyscalls = RealSyscalls;

#[cfg(test)]
static SYSCALLS: tests::support::MockSyscalls = tests::support::MockSyscalls;

struct RealSyscalls;

impl Syscalls for RealSyscalls {
    fn sendmsg(
        &self,
        socket: libc::c_int,
        msg: &libc::msghdr,
        flags: libc::c_int,
    ) -> std::io::Result<usize> {
        let sent = unsafe { libc::sendmsg(socket, msg, flags) };
        if sent < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(sent as usize)
        }
    }

    fn recvmsg(
        &self,
        socket: libc::c_int,
        msg: &mut libc::msghdr,
        flags: libc::c_int,
    ) -> std::io::Result<usize> {
        let received = unsafe { libc::recvmsg(socket, msg, flags) };
        if received < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(received as usize)
        }
    }

    fn fcntl_setfd_cloexec(&self, fd: libc::c_int) -> std::io::Result<()> {
        let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        if rc != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
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
            static FAIL_OP_UNIX: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) };
        }

        pub(in crate::channels::local) fn set_fail(op: &'static str) {
            FAIL_OP_UNIX.with(|cell| {
                *cell.borrow_mut() = Some(op);
            });
        }

        fn should_fail(op: &'static str) -> bool {
            FAIL_OP_UNIX.with(|cell| {
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
            fn sendmsg(
                &self,
                socket: libc::c_int,
                msg: &libc::msghdr,
                flags: libc::c_int,
            ) -> std::io::Result<usize> {
                if should_fail("sendmsg") {
                    Err(std::io::Error::other("sendmsg failpoint"))
                } else {
                    RealSyscalls.sendmsg(socket, msg, flags)
                }
            }

            fn recvmsg(
                &self,
                socket: libc::c_int,
                msg: &mut libc::msghdr,
                flags: libc::c_int,
            ) -> std::io::Result<usize> {
                if should_fail("recvmsg") {
                    Err(std::io::Error::other("recvmsg failpoint"))
                } else {
                    RealSyscalls.recvmsg(socket, msg, flags)
                }
            }

            fn fcntl_setfd_cloexec(&self, fd: libc::c_int) -> std::io::Result<()> {
                if should_fail("fcntl_cloexec") {
                    Err(std::io::Error::other("fcntl failpoint"))
                } else {
                    RealSyscalls.fcntl_setfd_cloexec(fd)
                }
            }
        }
    }

    #[test]
    fn unix_handle_send_failpoint_is_reported() {
        support::set_fail("sendmsg");

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
            .expect_err("sendmsg failpoint must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "sendmsg",
                ..
            }
        ));
    }

    #[test]
    fn endpoint_address_uses_private_runtime_directory() {
        let channel_id = ChannelId::new("channel-0").expect("channel id");
        let address = EndpointAddress::from_channel(&channel_id);

        assert!(
            address.as_str().contains("lava-flow"),
            "expected unix endpoint under lava-flow runtime directory, got {}",
            address.as_str()
        );
        assert!(
            address.as_str().ends_with("channel-0.sock"),
            "expected unix endpoint to end with socket filename, got {}",
            address.as_str()
        );
    }
}
