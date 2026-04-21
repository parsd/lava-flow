use super::{channel_invalid_input, channel_transport_error};
use crate::error::Result;
use crate::memory::allocator::InterprocessMemoryHandle;
use crate::types::ChannelId;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndpointAddress(String);

impl EndpointAddress {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_channel(channel_id: &ChannelId) -> Self {
        Self(format!("/tmp/lava-flow-{}.sock", channel_id.as_str()))
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug)]
pub(super) struct TransportSender {
    stream: UnixStream,
}

impl TransportSender {
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
            InterprocessMemoryHandle::CpuSharedFd(fd) => fd.into_raw_fd(),
            InterprocessMemoryHandle::GpuOpaqueFd(_) => {
                return Err(channel_invalid_input("unexpected gpu handle for cpu ipc"));
            }
        };

        let send_result = send_fd(self.stream.as_raw_fd(), fd);
        let _ = unsafe { libc::close(fd) };
        send_result
    }
}

#[derive(Debug)]
pub(super) struct TransportListener {
    listener: UnixListener,
    path: PathBuf,
}

impl TransportListener {
    pub(super) fn bind(address: &EndpointAddress) -> Result<Self> {
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

    pub(super) fn recv_cpu_handle(&mut self) -> Result<InterprocessMemoryHandle> {
        recv_fd(self.stream.as_raw_fd()).map(InterprocessMemoryHandle::from_cpu_shared_fd)
    }
}

fn send_fd(socket: libc::c_int, fd: libc::c_int) -> Result<()> {
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

    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Err(channel_invalid_input("missing cmsg header"));
        }
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len =
            libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as libc::c_uint) as usize;
        *(libc::CMSG_DATA(cmsg).cast::<libc::c_int>()) = fd;
    }

    SYSCALLS
        .sendmsg(socket, &msg, 0)
        .map(|_| ())
        .map_err(|source| channel_transport_error("sendmsg", source))
}

fn recv_fd(socket: libc::c_int) -> Result<OwnedFd> {
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
        .recvmsg(socket, &mut msg, 0)
        .map_err(|source| channel_transport_error("recvmsg", source))?;

    let fd = unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null() {
            return Err(channel_invalid_input("missing received handle"));
        }
        if (*cmsg).cmsg_level != libc::SOL_SOCKET || (*cmsg).cmsg_type != libc::SCM_RIGHTS {
            return Err(channel_invalid_input("unexpected received cmsg type"));
        }
        *(libc::CMSG_DATA(cmsg).cast::<libc::c_int>())
    };

    SYSCALLS
        .fcntl_setfd_cloexec(fd)
        .map_err(|source| channel_transport_error("fcntl_cloexec", source))?;

    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok(owned)
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
}
