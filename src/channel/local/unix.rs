use super::{Access, FrameKind, channel_protocol_error, channel_transport_error};
use crate::error::{LavaFlowError, Result};
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
    pub(crate) fn from_channel(channel_id: &ChannelId, access: Access) -> Self {
        let base = match runtime_dir_path(access) {
            Some(path) => path,
            None => PathBuf::from("/run/user")
                .join(unsafe { libc::geteuid() }.to_string())
                .join("lava-flow"),
        };
        let filename = match access {
            Access::CurrentSession => format!("{}.sock", channel_id.as_str()),
            Access::AuthenticatedUsers => format!("lava-flow-{}.sock", channel_id.as_str()),
        };
        let path = base.join(filename);
        Self(path.to_string_lossy().into_owned())
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

    pub(super) fn send_handle(&mut self, handle: InterprocessMemoryHandle) -> Result<()> {
        match handle {
            InterprocessMemoryHandle::CpuSharedFd(fd)
            | InterprocessMemoryHandle::GpuOpaqueFd(fd) => self.send_fd(&fd),
        }
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
    pub(super) fn bind(address: &EndpointAddress, access: Access) -> Result<Self> {
        let path = PathBuf::from(address.as_str());
        Self::ensure_endpoint_dir_exists(&path, access)?;
        if let Err(source) = fs::remove_file(&path)
            && source.kind() != std::io::ErrorKind::NotFound
        {
            return Err(channel_transport_error("remove_file", source));
        }
        let listener = match UnixListener::bind(&path) {
            Ok(listener) => listener,
            Err(source) => return Err(channel_transport_error("bind", source)),
        };
        if access == Access::AuthenticatedUsers {
            // Socket node permissions are rw-rw-rw-; directory sticky-bit ownership protects
            // unlink/replace while allowing authenticated local users to connect.
            if let Err(source) = fs::set_permissions(&path, fs::Permissions::from_mode(0o666)) {
                return Err(channel_transport_error("set_permissions", source));
            }
        }
        Ok(Self { listener, path })
    }

    fn ensure_endpoint_dir_exists(path: &std::path::Path, access: Access) -> Result<()> {
        let Some(directory) = path.parent() else {
            return Err(channel_protocol_error(
                "create_dir_all",
                "unix endpoint path has no parent directory",
            ));
        };
        if let Err(source) = fs::create_dir_all(directory) {
            return Err(channel_transport_error("create_dir_all", source));
        }
        match access {
            Access::CurrentSession => {
                // Validate ownership/type before changing permissions so we fail closed if some
                // other user pre-created the directory or replaced it with a symlink.
                Self::validate_runtime_dir(directory)?;
                // Runtime directory permissions are rwx------. Directories need execute/search
                // permission for the owner to traverse the path and create/remove the socket.
                match fs::set_permissions(directory, fs::Permissions::from_mode(0o700)) {
                    Ok(()) => {}
                    Err(source) => return Err(channel_transport_error("set_permissions", source)),
                }
                // Re-check after chmod so the caller knows the final runtime directory is actually private.
                Self::validate_private_runtime_dir(directory)
            }
            Access::AuthenticatedUsers => {
                Self::validate_public_runtime_dir(directory, Access::AuthenticatedUsers)
            }
        }
    }

    fn runtime_dir_metadata(directory: &Path) -> Result<fs::Metadata> {
        match fs::symlink_metadata(directory) {
            Ok(metadata) => Ok(metadata),
            Err(source) => Err(channel_transport_error("validate_runtime_dir", source)),
        }
    }

    fn runtime_dir_permission_error(message: &'static str) -> LavaFlowError {
        channel_transport_error(
            "validate_runtime_dir",
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, message),
        )
    }

    fn validate_runtime_dir(directory: &Path) -> Result<fs::Metadata> {
        let metadata = Self::runtime_dir_metadata(directory)?;
        if metadata.file_type().is_symlink() {
            return Err(Self::runtime_dir_permission_error(
                "runtime directory must not be a symlink",
            ));
        }
        if !metadata.is_dir() {
            return Err(Self::runtime_dir_permission_error(
                "runtime directory must be a directory",
            ));
        }
        let euid = unsafe { libc::geteuid() };
        if std::os::unix::fs::MetadataExt::uid(&metadata) != euid {
            return Err(Self::runtime_dir_permission_error(
                "runtime directory must be owned by the effective user",
            ));
        }
        Ok(metadata)
    }

    fn validate_private_runtime_dir(directory: &Path) -> Result<()> {
        let metadata = Self::validate_runtime_dir(directory)?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Self::runtime_dir_permission_error(
                "runtime directory permissions must not grant group or other access",
            ));
        }
        Ok(())
    }

    fn validate_public_runtime_dir(directory: &Path, access: Access) -> Result<()> {
        if access == Access::CurrentSession {
            return Self::validate_private_runtime_dir(directory);
        }

        let metadata = Self::runtime_dir_metadata(directory)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Self::runtime_dir_permission_error(
                "public runtime path must be a non-symlink directory",
            ));
        }
        let mode = metadata.permissions().mode();
        if mode & 0o002 == 0 || mode & 0o1000 == 0 {
            return Err(Self::runtime_dir_permission_error(
                "public runtime directory must be sticky and writable by other users",
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

    pub(super) fn try_accept(&mut self) -> Result<Option<TransportSender>> {
        if let Err(source) = self.listener.set_nonblocking(true) {
            return Err(channel_transport_error("set_nonblocking", source));
        }
        match self.listener.accept() {
            Ok((stream, _)) => {
                let listener_result = self.listener.set_nonblocking(false);
                let stream_result = stream.set_nonblocking(false);
                if let Err(source) = listener_result {
                    return Err(channel_transport_error("set_nonblocking", source));
                }
                if let Err(source) = stream_result {
                    return Err(channel_transport_error("set_nonblocking", source));
                }
                Ok(Some(TransportSender { stream }))
            }
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                if let Err(source) = self.listener.set_nonblocking(false) {
                    return Err(channel_transport_error("set_nonblocking", source));
                }
                Ok(None)
            }
            Err(source) => {
                let _ = self.listener.set_nonblocking(false);
                Err(channel_transport_error("accept", source))
            }
        }
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

    pub(super) fn recv_handle(&mut self, kind: FrameKind) -> Result<InterprocessMemoryHandle> {
        let fd = self.recv_fd()?;
        match kind {
            FrameKind::Cpu => Ok(InterprocessMemoryHandle::from_cpu_shared_fd(fd)),
            FrameKind::Gpu => Ok(InterprocessMemoryHandle::from_gpu_external_fd(fd)),
        }
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

fn runtime_dir_path(access: Access) -> Option<PathBuf> {
    if access == Access::AuthenticatedUsers {
        return Some(env::temp_dir());
    }

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

pub(super) fn is_retryable_connect_error(operation: &'static str, source: &std::io::Error) -> bool {
    operation == "connect"
        && matches!(
            source.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )
}

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
pub(crate) struct TestRuntimeDirGuard {
    _guard: crate::test_support::env::Guard,
}

#[cfg(test)]
pub(crate) fn stable_test_runtime_dir_guard(test_name: &str, id: u64) -> TestRuntimeDirGuard {
    let runtime_dir = env::temp_dir()
        .join(format!("lava-flow-public-channel-{test_name}-{id}"))
        .to_string_lossy()
        .into_owned();
    TestRuntimeDirGuard {
        _guard: crate::test_support::env::Guard::set(RUNTIME_DIR_OVERRIDE_ENV, &runtime_dir),
    }
}

#[cfg(test)]
pub(in crate::channel::local) mod tests {
    use super::super::tests::support::{
        BUFFER_SIZE, TestMeta, test_allocator, test_pair, test_transport_pair,
    };
    use super::super::{ProtocolLimits, ProtocolTag, Receiver};
    use super::*;
    use crate::test_support::env::Guard as EnvGuard;
    use crate::test_support::fs::{TempDir, TempFile};
    use crate::{channel::MetadataEncoding, error::LavaFlowError};
    use std::collections::BTreeMap;
    use std::os::fd::FromRawFd;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(in crate::channel::local) mod support {
        use super::*;

        thread_local! {
            static FAIL_OP_UNIX: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) };
        }

        pub(in crate::channel::local) fn set_fail(op: &'static str) {
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

        pub(in crate::channel::local) fn test_address() -> EndpointAddress {
            static COUNTER: AtomicU64 = AtomicU64::new(1);

            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let channel_id = ChannelId::new(format!("unix-channel-{id}")).expect("channel id");
            let path = env::temp_dir()
                .join(format!("lava-flow-tests-{}", std::process::id()))
                .join(format!("{}.sock", channel_id.as_str()));
            EndpointAddress(path.to_string_lossy().into_owned())
        }

        pub(in crate::channel::local) struct MockSyscalls;

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

    use support::test_address;

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
    fn retry_classifier_accepts_only_startup_race_connect_errors() {
        let not_found = std::io::Error::new(std::io::ErrorKind::NotFound, "listener not ready");
        let refused =
            std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "listener not ready");
        let denied = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");

        assert!(is_retryable_connect_error("connect", &not_found));
        assert!(is_retryable_connect_error("connect", &refused));
        assert!(!is_retryable_connect_error("connect", &denied));
        assert!(!is_retryable_connect_error("CreateFileW", &not_found));
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
    fn bind_reports_remove_file_failure_when_socket_path_is_directory() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base =
            TempDir::create(std::env::temp_dir().join(format!("lava-flow-unix-bind-test-{id}")))
                .expect("create test base directory");
        let base_string = base.path().to_string_lossy().into_owned();
        let _guard = EnvGuard::set("XDG_RUNTIME_DIR", &base_string);

        let socket_path = runtime_dir_path(Access::CurrentSession)
            .expect("resolve runtime dir")
            .join("permissions-probe.sock");
        TransportListener::ensure_endpoint_dir_exists(&socket_path, Access::CurrentSession)
            .expect("create runtime directory");

        let channel_id = ChannelId::new("bind-remove-file-error").expect("channel id");
        let address = EndpointAddress::from_channel(&channel_id, Access::CurrentSession);
        fs::create_dir_all(address.as_str()).expect("create blocking directory at socket path");

        let err = TransportListener::bind(&address, Access::CurrentSession)
            .expect_err("bind must fail when existing socket path is a directory");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "remove_file",
                ..
            }
        ));
    }

    #[test]
    fn endpoint_address_uses_private_runtime_directory() {
        let channel_id = ChannelId::new("channel-0").expect("channel id");
        let address = EndpointAddress::from_channel(&channel_id, Access::CurrentSession);

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
    fn authenticated_users_endpoint_uses_system_temp_directory() {
        let channel_id = ChannelId::new("auth-users-channel").expect("channel id");
        let address = EndpointAddress::from_channel(&channel_id, Access::AuthenticatedUsers);
        let expected = std::env::temp_dir().join("lava-flow-auth-users-channel.sock");

        assert_eq!(PathBuf::from(address.as_str()), expected);
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
        let base =
            TempDir::create(std::env::temp_dir().join(format!("lava-flow-unix-runtime-test-{id}")))
                .expect("create test base directory");
        let base_string = base.path().to_string_lossy().into_owned();
        let _guard = EnvGuard::set("XDG_RUNTIME_DIR", &base_string);

        let socket_path = runtime_dir_path(Access::CurrentSession)
            .expect("resolve runtime dir")
            .join("permissions-probe.sock");
        TransportListener::ensure_endpoint_dir_exists(&socket_path, Access::CurrentSession)
            .expect("create runtime directory");

        let runtime_dir = runtime_dir_path(Access::CurrentSession).expect("resolve runtime dir");
        let metadata = fs::metadata(&runtime_dir).expect("read runtime directory metadata");
        let mode = metadata.permissions().mode() & 0o777;

        assert_eq!(
            mode, 0o700,
            "expected private runtime directory permissions, got {:o}",
            mode,
        );
    }

    #[test]
    fn authenticated_users_bind_sets_socket_permissions_for_local_users() {
        let channel_id = ChannelId::new(format!("auth-users-socket-{}", std::process::id()))
            .expect("channel id");
        let address = EndpointAddress::from_channel(&channel_id, Access::AuthenticatedUsers);
        let listener = TransportListener::bind(&address, Access::AuthenticatedUsers)
            .expect("bind authenticated-users listener");

        let metadata = fs::metadata(address.as_str()).expect("read socket metadata");
        assert_eq!(metadata.permissions().mode() & 0o777, 0o666);

        drop(listener);
    }

    #[test]
    fn validate_runtime_dir_accepts_owned_private_directory() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory =
            TempDir::create(std::env::temp_dir().join(format!("lava-flow-validate-runtime-{id}")))
                .expect("create runtime directory");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("set private permissions");

        TransportListener::validate_private_runtime_dir(directory.path())
            .expect("owned private directory must be accepted");
    }

    #[test]
    fn validate_runtime_dir_rejects_regular_file() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("lava-flow-runtime-file-{id}"));
        let file = TempFile::with_contents(path, b"not a directory").expect("create runtime file");

        let err = TransportListener::validate_runtime_dir(file.path())
            .expect_err("regular file must not be accepted as runtime dir");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "validate_runtime_dir",
                ..
            }
        ));
    }

    #[test]
    fn validate_private_runtime_dir_rejects_group_or_other_permissions() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory =
            TempDir::create(std::env::temp_dir().join(format!("lava-flow-public-runtime-{id}")))
                .expect("create runtime directory");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755))
            .expect("set public permissions");

        let err = TransportListener::validate_private_runtime_dir(directory.path())
            .expect_err("public runtime directory permissions must be rejected");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "validate_runtime_dir",
                ..
            }
        ));
    }

    #[test]
    fn validate_public_runtime_dir_rejects_regular_file() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("lava-flow-public-runtime-file-{id}"));
        let file =
            TempFile::with_contents(path, b"not a directory").expect("create public runtime file");

        let err =
            TransportListener::validate_public_runtime_dir(file.path(), Access::AuthenticatedUsers)
                .expect_err("regular file must not be accepted as public runtime dir");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "validate_runtime_dir",
                ..
            }
        ));
    }

    #[test]
    fn validate_public_runtime_dir_rejects_non_sticky_directory() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = TempDir::create(
            std::env::temp_dir().join(format!("lava-flow-public-runtime-private-{id}")),
        )
        .expect("create public runtime directory");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .expect("set private permissions");

        let err = TransportListener::validate_public_runtime_dir(
            directory.path(),
            Access::AuthenticatedUsers,
        )
        .expect_err("non-sticky private dir must not be accepted as public runtime dir");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "validate_runtime_dir",
                ..
            }
        ));
    }

    #[test]
    fn ensure_endpoint_dir_exists_reports_create_dir_all_failure() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("lava-flow-runtime-parent-file-{id}"));
        let file =
            TempFile::with_contents(path, b"not a directory").expect("create blocking parent file");
        let socket_path = file.path().join("blocked.sock");

        let err =
            TransportListener::ensure_endpoint_dir_exists(&socket_path, Access::AuthenticatedUsers)
                .expect_err("file parent must make create_dir_all fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "create_dir_all",
                ..
            }
        ));
    }

    #[test]
    fn bind_reports_os_bind_failure_for_too_long_socket_path() {
        static COUNTER: AtomicU64 = AtomicU64::new(1);

        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let base =
            TempDir::create(std::env::temp_dir().join(format!("lava-flow-long-socket-{id}")))
                .expect("create long socket base");
        let path = base.path().join(format!("{}.sock", "a".repeat(200)));
        let address = EndpointAddress(path.to_string_lossy().into_owned());

        let err = TransportListener::bind(&address, Access::CurrentSession)
            .expect_err("overlong socket path must fail at bind");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "bind",
                ..
            }
        ));
    }

    #[test]
    fn home_dir_for_uid_prefers_home_env() {
        let uid = unsafe { libc::geteuid() };
        let home = std::env::temp_dir().join("lava-flow-home-env");
        let home_string = home.to_string_lossy().into_owned();
        let _guard = EnvGuard::set("HOME", &home_string);

        assert_eq!(home_dir_for_uid(uid), Some(home));
    }

    #[test]
    fn home_dir_for_uid_falls_back_to_passwd_when_home_is_unset() {
        let uid = unsafe { libc::geteuid() };
        let _guard = EnvGuard::unset("HOME");

        assert_eq!(home_dir_for_uid(uid), home_dir_from_passwd(uid));
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
        let base =
            TempDir::create(std::env::temp_dir().join(format!("lava-flow-unix-symlink-test-{id}")))
                .expect("create test base directory");
        let target = base.path().join("target");
        let runtime_link = base.path().join("runtime-link");
        fs::create_dir_all(&target).expect("create target directory");
        std::os::unix::fs::symlink(&target, &runtime_link).expect("create runtime symlink");
        let runtime_string = runtime_link.to_string_lossy().into_owned();
        let _guard = EnvGuard::set(RUNTIME_DIR_OVERRIDE_ENV, &runtime_string);

        let channel_id = ChannelId::new("symlink-runtime-dir").expect("channel id");
        let address = EndpointAddress::from_channel(&channel_id, Access::CurrentSession);

        let err = TransportListener::bind(&address, Access::CurrentSession)
            .expect_err("bind must reject symlinked runtime directory");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "validate_runtime_dir",
                ..
            }
        ));
    }

    #[test]
    fn ensure_endpoint_dir_exists_rejects_path_without_parent_directory() {
        let err = TransportListener::ensure_endpoint_dir_exists(
            std::path::Path::new("/"),
            Access::CurrentSession,
        )
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
        let address = test_address();
        let listener = TransportListener::bind(&address, Access::CurrentSession)
            .expect("bind transport listener");
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
    fn try_accept_returns_none_without_connected_receiver() {
        let address = test_address();
        let mut listener = TransportListener::bind(&address, Access::CurrentSession)
            .expect("bind transport listener");

        assert!(
            listener
                .try_accept()
                .expect("try_accept without receiver should not fail")
                .is_none()
        );
    }

    #[test]
    fn try_accept_restores_blocking_mode_after_empty_poll() {
        let address = test_address();
        let mut listener = TransportListener::bind(&address, Access::CurrentSession)
            .expect("bind transport listener");

        assert!(
            listener
                .try_accept()
                .expect("empty try_accept should not fail")
                .is_none()
        );

        let receiver_address = address.clone();
        let accept_thread = thread::spawn(move || {
            let mut sender = listener.accept().expect("blocking accept should wait");
            sender
                .write_all(&[0xFA])
                .expect("write accepted sender byte");
            sender.flush().expect("flush accepted sender byte");
        });
        thread::sleep(std::time::Duration::from_millis(50));

        let mut receiver =
            TransportReceiver::connect(&receiver_address).expect("connect transport receiver");
        let mut byte = [0_u8; 1];
        receiver
            .read_exact(&mut byte)
            .expect("read accepted sender byte");

        accept_thread.join().expect("accept thread must not panic");
        assert_eq!(byte[0], 0xFA);
    }

    #[test]
    fn try_accept_accepts_connected_receiver() {
        let address = test_address();
        let mut listener = TransportListener::bind(&address, Access::CurrentSession)
            .expect("bind transport listener");
        let receiver_address = address.clone();
        let receiver_thread = thread::spawn(move || {
            let mut receiver =
                TransportReceiver::connect(&receiver_address).expect("connect transport receiver");
            let mut byte = [0_u8; 1];
            receiver
                .read_exact(&mut byte)
                .expect("read accepted sender byte");
            byte[0]
        });

        let mut sender = loop {
            if let Some(sender) = listener.try_accept().expect("poll listener") {
                break sender;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        };
        sender.write_all(&[0xEF]).expect("write sender byte");
        sender.flush().expect("flush sender byte");

        assert_eq!(
            receiver_thread
                .join()
                .expect("receiver thread must not panic"),
            0xEF
        );
    }

    #[test]
    fn transport_sender_reports_read_error_after_receiver_drop() {
        let (mut sender, receiver) =
            super::super::tests::support::test_transport_pair().expect("create transport pair");
        drop(receiver);

        let mut byte = [0_u8; 1];
        let err = sender
            .read_exact(&mut byte)
            .expect_err("read from closed receiver socket must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation { .. } | LavaFlowError::ChannelDisconnected
        ));
    }

    #[test]
    fn transport_receiver_reports_write_error_after_sender_drop() {
        let (sender, mut receiver) =
            super::super::tests::support::test_transport_pair().expect("create transport pair");
        drop(sender);

        let err = receiver
            .write_all(&[0xAA])
            .and_then(|()| receiver.flush())
            .expect_err("write or flush to closed sender socket must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation { .. } | LavaFlowError::ChannelDisconnected
        ));
    }

    #[test]
    fn recv_handle_reports_recvmsg_failpoint() {
        support::set_fail("recvmsg");

        let (_sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");

        let err = receiver_transport
            .recv_handle(FrameKind::Cpu)
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
    fn recv_handle_reports_fcntl_failpoint() {
        support::set_fail("fcntl_cloexec");

        let (sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .send_fd(&pipe_write_end())
            .expect("send cpu handle fd");

        let err = receiver_transport
            .recv_handle(FrameKind::Cpu)
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
    fn recv_handle_rejects_missing_received_handle() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[0])
            .expect("write marker without ancillary data");
        sender_transport.flush().expect("flush marker");

        let err = receiver_transport
            .recv_handle(FrameKind::Cpu)
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
    fn recv_handle_rejects_unexpected_received_cmsg_type() {
        support::set_fail("recvmsg_unexpected_cmsg_type");

        let (sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .send_fd(&pipe_write_end())
            .expect("send cpu handle fd");

        let err = receiver_transport
            .recv_handle(FrameKind::Cpu)
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
    fn recv_handle_rejects_missing_received_handle_with_failpoint() {
        support::set_fail("recvmsg_missing_handle");

        let (sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .send_fd(&pipe_write_end())
            .expect("send cpu handle fd");

        let err = receiver_transport
            .recv_handle(FrameKind::Cpu)
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
        let mut receiver = Receiver::new(
            MetadataEncoding::Json,
            receiver_transport,
            ProtocolLimits::default(),
        );
        let metadata = serde_json::to_vec(&TestMeta {
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
        let mut receiver = Receiver::new(
            MetadataEncoding::Json,
            receiver_transport,
            ProtocolLimits::default(),
        );
        let metadata = serde_json::to_vec(&crate::channel::MessageMeta {
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
