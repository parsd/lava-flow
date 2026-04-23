use super::{FrameKind, channel_protocol_error, channel_transport_error};
use crate::error::Result;
use crate::memory::allocator::InterprocessMemoryHandle;
use crate::types::ChannelId;
use std::env;
use std::ffi::{CStr, OsStr, OsString};
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::thread;

const RUNTIME_DIR_OVERRIDE_ENV: &str = "LAVA_FLOW_RUNTIME_DIR";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndpointAddress(String);

impl EndpointAddress {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_channel(channel_id: &ChannelId) -> Self {
        let base = match runtime_dir_path() {
            Some(path) => path,
            None => PathBuf::from("/run/user")
                .join(unsafe { libc::geteuid() }.to_string())
                .join("lava-flow"),
        };
        let path = base.join(format!("{}.sock", channel_id.as_str()));
        Self(path.to_string_lossy().into_owned())
    }

    #[cfg(test)]
    pub(in crate::channels::local) fn from_test_channel(channel_id: &ChannelId) -> Self {
        let path = env::temp_dir()
            .join(format!("lava-flow-tests-{}", std::process::id()))
            .join(format!("{}.sock", channel_id.as_str()));
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
        match self.stream.read_exact(bytes) {
            Ok(()) => Ok(()),
            Err(source) => Err(channel_transport_error("read_exact", source)),
        }
    }

    pub(super) fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        match self.stream.write_all(bytes) {
            Ok(()) => Ok(()),
            Err(source) => Err(channel_transport_error("write_all", source)),
        }
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        match self.stream.flush() {
            Ok(()) => Ok(()),
            Err(source) => Err(channel_transport_error("flush", source)),
        }
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
        let path = PathBuf::from(address.as_str());
        Self::ensure_endpoint_dir_exists(&path)?;
        if let Err(source) = fs::remove_file(&path)
            && source.kind() != std::io::ErrorKind::NotFound
        {
            return Err(channel_transport_error("remove_file", source));
        }
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(source) => return Err(channel_transport_error("bind", source)),
        };
        Ok(Self { listener, path })
    }

    fn ensure_endpoint_dir_exists(path: &std::path::Path) -> Result<()> {
        let Some(directory) = path.parent() else {
            return Err(channel_protocol_error(
                "create_dir_all",
                "unix endpoint path has no parent directory",
            ));
        };
        if let Err(source) = fs::create_dir_all(directory) {
            return Err(channel_transport_error("create_dir_all", source));
        }
        // Validate ownership/type before changing permissions so we fail closed if some other
        // user pre-created the directory or replaced it with a symlink.
        Self::validate_runtime_dir(directory)?;
        match fs::set_permissions(directory, fs::Permissions::from_mode(0o700)) {
            Ok(()) => {}
            Err(source) => return Err(channel_transport_error("set_permissions", source)),
        }
        // Re-check after chmod so the caller knows the final runtime directory is actually private.
        Self::validate_private_runtime_dir(directory)
    }

    fn validate_runtime_dir(directory: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(directory)
            .map_err(|source| channel_transport_error("validate_runtime_dir", source))?;
        if metadata.file_type().is_symlink() {
            return Err(channel_transport_error(
                "validate_runtime_dir",
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "runtime directory must not be a symlink",
                ),
            ));
        }
        if !metadata.is_dir() {
            return Err(channel_transport_error(
                "validate_runtime_dir",
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "runtime directory must be a directory",
                ),
            ));
        }
        let euid = unsafe { libc::geteuid() };
        if std::os::unix::fs::MetadataExt::uid(&metadata) != euid {
            return Err(channel_transport_error(
                "validate_runtime_dir",
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "runtime directory must be owned by the effective user",
                ),
            ));
        }
        Ok(())
    }

    fn validate_private_runtime_dir(directory: &Path) -> Result<()> {
        Self::validate_runtime_dir(directory)?;
        let metadata = fs::symlink_metadata(directory)
            .map_err(|source| channel_transport_error("validate_runtime_dir", source))?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(channel_transport_error(
                "validate_runtime_dir",
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "runtime directory permissions must not grant group or other access",
                ),
            ));
        }
        Ok(())
    }

    pub(super) fn accept(self) -> Result<TransportSender> {
        let (stream, _) = match self.listener.accept() {
            Ok(connection) => connection,
            Err(source) => return Err(channel_transport_error("accept", source)),
        };
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
        let stream = match UnixStream::connect(address.as_str()) {
            Ok(stream) => stream,
            Err(source) => return Err(channel_transport_error("connect", source)),
        };
        Ok(Self { stream })
    }

    pub(super) fn read_exact(&mut self, bytes: &mut [u8]) -> Result<()> {
        match self.stream.read_exact(bytes) {
            Ok(()) => Ok(()),
            Err(source) => Err(channel_transport_error("read_exact", source)),
        }
    }

    pub(super) fn write_all(&mut self, bytes: &[u8]) -> Result<()> {
        match self.stream.write_all(bytes) {
            Ok(()) => Ok(()),
            Err(source) => Err(channel_transport_error("write_all", source)),
        }
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        match self.stream.flush() {
            Ok(()) => Ok(()),
            Err(source) => Err(channel_transport_error("flush", source)),
        }
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

fn runtime_dir_path() -> Option<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    runtime_dir_path_with(
        env::var_os(RUNTIME_DIR_OVERRIDE_ENV),
        env::var_os("XDG_RUNTIME_DIR"),
        uid,
        PathBuf::from("/run/user").join(uid.to_string()).is_dir(),
        home_dir_for_uid(uid),
    )
}

fn runtime_dir_path_with(
    runtime_dir_override: Option<OsString>,
    xdg_runtime_dir: Option<OsString>,
    uid: libc::uid_t,
    run_user_exists: bool,
    home_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    // Prefer an explicit runtime-dir override first so containers can mount and pass a private
    // shared directory even when XDG_RUNTIME_DIR is unavailable.
    if let Some(path) = non_empty_env_path(runtime_dir_override) {
        return Some(path);
    }

    // Next prefer the standard per-user runtime directory when the environment provides it.
    if let Some(path) = non_empty_env_path(xdg_runtime_dir) {
        return Some(path.join("lava-flow"));
    }

    // Some stripped-down environments do not export XDG_RUNTIME_DIR but still provide the
    // standard /run/user/<uid> runtime base.
    let run_user = PathBuf::from("/run/user").join(uid.to_string());
    if run_user_exists {
        return Some(run_user.join("lava-flow"));
    }

    // Final fallback is under the user's home directory instead of /tmp so another user cannot
    // pre-create a predictable shared-temp parent and interfere with the socket path.
    home_dir.map(|home| home.join(".local").join("run").join("lava-flow"))
}

fn non_empty_env_path(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

fn home_dir_for_uid(uid: libc::uid_t) -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        // Container or stripped-down service environments do not always export HOME even though
        // the user still has a passwd entry with a stable home directory.
        .or_else(|| home_dir_from_passwd(uid))
}

fn home_dir_from_passwd(uid: libc::uid_t) -> Option<PathBuf> {
    // It lets the library derive a deterministic per-user runtime path even when HOME and
    // XDG_RUNTIME_DIR are both unset, which is common in minimal container/service setups.
    let passwd = unsafe { libc::getpwuid(uid) };
    if passwd.is_null() {
        return None;
    }

    let directory = unsafe { (*passwd).pw_dir };
    if directory.is_null() {
        return None;
    }

    let bytes = unsafe { CStr::from_ptr(directory) }.to_bytes();
    if bytes.is_empty() {
        None
    } else {
        Some(PathBuf::from(OsStr::from_bytes(bytes)))
    }
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
        BUFFER_SIZE, TestMeta, USED_SIZE, test_allocator, test_pair, test_transport_pair,
    };
    use super::super::{CpuReceiver, LocalProtocolLimits, ProtocolTag};
    use super::*;
    use crate::test_support::env::Guard as EnvGuard;
    use crate::{channels::MetadataEncoding, error::LavaFlowError};
    use std::collections::BTreeMap;
    use std::os::fd::FromRawFd;
    use std::sync::atomic::{AtomicU64, Ordering};

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
                } else if should_fail("recvmsg_missing_handle") {
                    let received = RealSyscalls.recvmsg(socket, msg, flags)?;
                    msg.msg_controllen = 0;
                    Ok(received)
                } else if should_fail("recvmsg_unexpected_cmsg_type") {
                    let received = RealSyscalls.recvmsg(socket, msg, flags)?;
                    unsafe {
                        let cmsg = libc::CMSG_FIRSTHDR(msg);
                        if !cmsg.is_null() {
                            (*cmsg).cmsg_type = 0;
                        }
                    }
                    Ok(received)
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

    fn pipe_write_end() -> OwnedFd {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "create pipe");
        let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        drop(read_end);
        write_end
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
    fn send_cpu_handle_rejects_gpu_handle_for_cpu_ipc() {
        let (mut sender_transport, _receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let handle = InterprocessMemoryHandle::from_gpu_external_fd(pipe_write_end());

        let err = sender_transport
            .send_cpu_handle(handle)
            .expect_err("gpu handle must be rejected for cpu ipc");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "send_cpu_handle",
                ..
            }
        ));
    }

    #[test]
    fn bind_reports_remove_file_failure_when_socket_path_is_directory() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("lava-flow-unix-bind-test-{id}"));
        let _cleanup_before = fs::remove_dir_all(&base);
        let base_string = base.to_string_lossy().into_owned();
        let _guard = EnvGuard::set("XDG_RUNTIME_DIR", &base_string);

        let socket_path = runtime_dir_path()
            .expect("resolve runtime dir")
            .join("permissions-probe.sock");
        TransportListener::ensure_endpoint_dir_exists(&socket_path)
            .expect("create runtime directory");

        let channel_id = ChannelId::new("bind-remove-file-error").expect("channel id");
        let address = EndpointAddress::from_channel(&channel_id);
        fs::create_dir_all(address.as_str()).expect("create blocking directory at socket path");

        let err = TransportListener::bind(&address)
            .expect_err("bind must fail when existing socket path is a directory");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "remove_file",
                ..
            }
        ));

        let _cleanup_after = fs::remove_dir_all(&base);
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

    #[test]
    fn runtime_directory_prefers_explicit_override_over_xdg() {
        let path = runtime_dir_path_with(
            Some(OsString::from("/override/runtime")),
            Some(OsString::from("/xdg/runtime")),
            1000,
            false,
            Some(PathBuf::from("/home/test-user")),
        )
        .expect("resolve runtime dir");

        assert_eq!(path, PathBuf::from("/override/runtime"));
    }

    #[test]
    fn runtime_directory_falls_back_to_home_when_xdg_and_run_user_are_unavailable() {
        let path = runtime_dir_path_with(
            None,
            None,
            1000,
            false,
            Some(PathBuf::from("/home/test-user")),
        )
        .expect("resolve home fallback runtime dir");

        assert_eq!(path, PathBuf::from("/home/test-user/.local/run/lava-flow"));
    }

    #[test]
    fn runtime_directory_uses_run_user_when_xdg_is_unavailable() {
        let path = runtime_dir_path_with(
            None,
            None,
            1234,
            true,
            Some(PathBuf::from("/home/test-user")),
        )
        .expect("resolve /run/user fallback runtime dir");

        assert_eq!(path, PathBuf::from("/run/user/1234/lava-flow"));
    }

    #[test]
    fn runtime_directory_is_created_with_0700_permissions() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("lava-flow-unix-runtime-test-{id}"));
        let _cleanup_before = fs::remove_dir_all(&base);
        let base_string = base.to_string_lossy().into_owned();
        let _guard = EnvGuard::set("XDG_RUNTIME_DIR", &base_string);

        let socket_path = runtime_dir_path()
            .expect("resolve runtime dir")
            .join("permissions-probe.sock");
        TransportListener::ensure_endpoint_dir_exists(&socket_path)
            .expect("create runtime directory");

        let runtime_dir = runtime_dir_path().expect("resolve runtime dir");
        let metadata = fs::metadata(&runtime_dir).expect("read runtime directory metadata");
        let mode = metadata.permissions().mode() & 0o777;

        assert_eq!(
            mode, 0o700,
            "expected private runtime directory permissions, got {:o}",
            mode,
        );

        let _cleanup_after = fs::remove_dir_all(&base);
    }

    #[test]
    fn validate_runtime_dir_accepts_owned_private_directory() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!("lava-flow-validate-runtime-{id}"));
        let _cleanup_before = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("create runtime directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("set private permissions");

        TransportListener::validate_private_runtime_dir(&directory)
            .expect("owned private directory must be accepted");

        let _cleanup_after = fs::remove_dir_all(&directory);
    }

    #[test]
    fn home_dir_from_passwd_returns_path_for_current_user() {
        let uid = unsafe { libc::geteuid() };
        let path = home_dir_from_passwd(uid).expect("passwd entry should provide a home directory");
        assert!(
            path.is_absolute(),
            "expected passwd home directory to be absolute, got {:?}",
            path
        );
    }

    #[test]
    fn bind_rejects_symlinked_runtime_directory() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("lava-flow-unix-symlink-test-{id}"));
        let target = base.join("target");
        let runtime_link = base.join("runtime-link");
        let _cleanup_before = fs::remove_dir_all(&base);
        fs::create_dir_all(&target).expect("create target directory");
        std::os::unix::fs::symlink(&target, &runtime_link).expect("create runtime symlink");
        let runtime_string = runtime_link.to_string_lossy().into_owned();
        let _guard = EnvGuard::set(RUNTIME_DIR_OVERRIDE_ENV, &runtime_string);

        let channel_id = ChannelId::new("symlink-runtime-dir").expect("channel id");
        let address = EndpointAddress::from_channel(&channel_id);

        let err = TransportListener::bind(&address)
            .expect_err("bind must reject symlinked runtime directory");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "validate_runtime_dir",
                ..
            }
        ));

        let _cleanup_after = fs::remove_dir_all(&base);
    }

    #[test]
    fn ensure_endpoint_dir_exists_rejects_path_without_parent_directory() {
        let err = TransportListener::ensure_endpoint_dir_exists(std::path::Path::new("/"))
            .expect_err("root path must be rejected because it has no parent endpoint directory");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "create_dir_all",
                ..
            }
        ));
    }

    #[test]
    fn transport_pair_supports_reverse_control_bytes_directly() {
        let address = super::super::tests::support::test_address();
        let listener = TransportListener::bind(&address).expect("bind transport listener");
        let receiver_address = address.clone();
        let receiver_thread = thread::spawn(move || {
            let mut receiver =
                TransportReceiver::connect(&receiver_address).expect("connect transport receiver");
            receiver
                .write_all(&[0xAB])
                .expect("write reverse control byte");
            receiver.flush().expect("flush reverse control byte");

            let mut reply = [0_u8; 1];
            receiver
                .read_exact(&mut reply)
                .expect("read sender reply byte");
            reply[0]
        });

        let mut sender = listener.accept().expect("accept transport sender");
        let mut received = [0_u8; 1];
        sender
            .read_exact(&mut received)
            .expect("read receiver control byte");
        assert_eq!(received[0], 0xAB);

        sender.write_all(&[0xCD]).expect("write sender reply byte");
        sender.flush().expect("flush sender reply byte");

        let reply = receiver_thread
            .join()
            .expect("receiver thread must not panic");
        assert_eq!(reply, 0xCD);
    }

    #[test]
    fn recv_cpu_handle_reports_recvmsg_failpoint() {
        support::set_fail("recvmsg");

        let (_sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");

        let err = receiver_transport
            .recv_cpu_handle()
            .expect_err("recvmsg failpoint must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "recvmsg",
                ..
            }
        ));
    }

    #[test]
    fn recv_cpu_handle_reports_fcntl_failpoint() {
        support::set_fail("fcntl_cloexec");

        let (sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .send_fd(&pipe_write_end())
            .expect("send cpu handle fd");

        let err = receiver_transport
            .recv_cpu_handle()
            .expect_err("fcntl failpoint must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "fcntl_cloexec",
                ..
            }
        ));
    }

    #[test]
    fn recv_cpu_handle_rejects_missing_received_handle() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[0])
            .expect("write marker without ancillary data");
        sender_transport.flush().expect("flush marker");

        let err = receiver_transport
            .recv_cpu_handle()
            .expect_err("missing ancillary data must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "recv_fd",
                ..
            }
        ));
    }

    #[test]
    fn recv_cpu_handle_rejects_unexpected_received_cmsg_type() {
        support::set_fail("recvmsg_unexpected_cmsg_type");

        let (sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .send_fd(&pipe_write_end())
            .expect("send cpu handle fd");

        let err = receiver_transport
            .recv_cpu_handle()
            .expect_err("unexpected cmsg type must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "recv_fd",
                ..
            }
        ));
    }

    #[test]
    fn recv_cpu_handle_rejects_missing_received_handle_with_failpoint() {
        support::set_fail("recvmsg_missing_handle");

        let (sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .send_fd(&pipe_write_end())
            .expect("send cpu handle fd");

        let err = receiver_transport
            .recv_cpu_handle()
            .expect_err("missing cmsg header must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "recv_fd",
                ..
            }
        ));
    }

    #[test]
    fn recv_reports_import_failure_and_sends_import_failed_ack_for_invalid_cpu_handle() {
        let (mut sender_transport, receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let mut receiver = CpuReceiver::new(
            MetadataEncoding::Json,
            receiver_transport,
            LocalProtocolLimits::default(),
        );
        let metadata = serde_json::to_vec(&TestMeta {
            used_size: USED_SIZE,
            width: 8,
            height: 8,
        })
        .expect("serialize metadata");

        sender_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8, FrameKind::Cpu as u8])
            .expect("write message tag");
        sender_transport
            .write_all(&(BUFFER_SIZE as u64).to_le_bytes())
            .expect("write payload size");
        sender_transport
            .send_fd(&pipe_write_end())
            .expect("send invalid cpu fd");
        sender_transport
            .write_all(&(metadata.len() as u32).to_le_bytes())
            .expect("write metadata length");
        sender_transport
            .write_all(&metadata)
            .expect("write metadata bytes");
        sender_transport.flush().expect("flush message");

        let err = receiver
            .recv::<TestMeta>()
            .expect_err("receiver must fail when cpu handle import is invalid");
        assert!(matches!(err, LavaFlowError::SharedMemoryOperation { .. }));

        let ack = ProtocolTag::read_from_sender(&mut sender_transport, "read_import_ack")
            .expect("read import-failed ack");
        assert_eq!(ack, ProtocolTag::ImportFailed);
    }

    #[test]
    fn recv_map_reports_import_failure_and_sends_import_failed_ack_for_invalid_cpu_handle() {
        let (mut sender_transport, receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let mut receiver = CpuReceiver::new(
            MetadataEncoding::Json,
            receiver_transport,
            LocalProtocolLimits::default(),
        );
        let metadata = serde_json::to_vec(&crate::channels::MessageMeta {
            used_size: USED_SIZE,
            values: BTreeMap::new(),
        })
        .expect("serialize metadata");

        sender_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8, FrameKind::Cpu as u8])
            .expect("write message tag");
        sender_transport
            .write_all(&(BUFFER_SIZE as u64).to_le_bytes())
            .expect("write payload size");
        sender_transport
            .send_fd(&pipe_write_end())
            .expect("send invalid cpu fd");
        sender_transport
            .write_all(&(metadata.len() as u32).to_le_bytes())
            .expect("write metadata length");
        sender_transport
            .write_all(&metadata)
            .expect("write metadata bytes");
        sender_transport.flush().expect("flush message");

        let err = receiver
            .recv_map()
            .expect_err("receiver must fail when cpu handle import is invalid");
        assert!(matches!(err, LavaFlowError::SharedMemoryOperation { .. }));

        let ack = ProtocolTag::read_from_sender(&mut sender_transport, "read_import_ack")
            .expect("read import-failed ack");
        assert_eq!(ack, ProtocolTag::ImportFailed);
    }

    #[test]
    fn real_syscalls_sendmsg_reports_os_error_for_invalid_socket() {
        let msg: libc::msghdr = unsafe { std::mem::zeroed() };
        let err = RealSyscalls
            .sendmsg(-1, &msg, 0)
            .expect_err("invalid socket must fail");
        assert!(
            err.raw_os_error().is_some(),
            "expected invalid socket to yield an OS error, got {err:?}"
        );
    }

    #[test]
    fn real_syscalls_recvmsg_reports_os_error_for_invalid_socket() {
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        let err = RealSyscalls
            .recvmsg(-1, &mut msg, 0)
            .expect_err("invalid socket must fail");
        assert!(
            err.raw_os_error().is_some(),
            "expected invalid socket to yield an OS error, got {err:?}"
        );
    }

    #[test]
    fn real_syscalls_fcntl_cloexec_reports_os_error_for_invalid_fd() {
        let err = RealSyscalls
            .fcntl_setfd_cloexec(-1)
            .expect_err("invalid fd must fail");
        assert!(
            err.raw_os_error().is_some(),
            "expected invalid fd to yield an OS error, got {err:?}"
        );
    }
}
