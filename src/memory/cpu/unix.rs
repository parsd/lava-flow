use super::*;

use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    ffi::CString,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
};

#[cfg_attr(not(test), allow(dead_code))]
trait Syscalls: Sync {
    fn shm_open(&self, name: &CString) -> Result<OwnedFd>;
    fn shm_unlink(&self, name: &CString) -> Result<()>;
    fn ftruncate(&self, fd: &OwnedFd, size: usize) -> Result<()>;
    fn mmap(&self, fd: &OwnedFd, size: usize) -> Result<*mut libc::c_void>;
    fn munmap(&self, addr: *mut libc::c_void, len: usize) -> Result<()>;
    fn fcntl_dupfd_cloexec(&self, fd: libc::c_int) -> Result<libc::c_int>;
    fn dup(&self, fd: libc::c_int) -> Result<libc::c_int>;
    fn fcntl_setfd_cloexec(&self, fd: libc::c_int) -> Result<()>;
    fn getrandom(&self, buf: *mut libc::c_void, len: usize, flags: libc::c_uint) -> Result<usize>;
    fn open(&self, path: *const libc::c_char, flags: libc::c_int) -> Result<OwnedFd>;
    fn read(&self, fd: libc::c_int, buf: *mut libc::c_void, count: usize) -> Result<usize>;

    fn dup_fd_cloexec(&self, fd: libc::c_int) -> Result<OwnedFd> {
        if let Ok(duplicated_fd) = self.fcntl_dupfd_cloexec(fd) {
            let owned_fd = unsafe { OwnedFd::from_raw_fd(duplicated_fd) };
            return Ok(owned_fd);
        }

        // Fallback path for older platforms lacking F_DUPFD_CLOEXEC support.
        let duplicated_fd = self.dup(fd)?;
        let owned_fd = unsafe { OwnedFd::from_raw_fd(duplicated_fd) };
        self.fcntl_setfd_cloexec(owned_fd.as_raw_fd())?;
        Ok(owned_fd)
    }

    fn random_u64(&self) -> u64 {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let mut out = 0_u64;
            let rc = self.getrandom(
                (&mut out as *mut u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
                0,
            );
            if matches!(rc, Ok(n) if n == std::mem::size_of::<u64>()) {
                return out;
            }
        }

        let mut out = 0_u64;
        let urandom = b"/dev/urandom\0";
        let fd = self.open(
            urandom.as_ptr().cast::<libc::c_char>(),
            // O_CLOEXEC: prevent fd inheritance across exec so child processes cannot retain access.
            libc::O_RDONLY | libc::O_CLOEXEC,
        );
        if let Ok(fd) = fd {
            let read_rc = self.read(
                fd.as_raw_fd(),
                (&mut out as *mut u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            );
            if matches!(read_rc, Ok(n) if n == std::mem::size_of::<u64>()) {
                return out;
            }
        }

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        nanos ^ ((std::process::id() as u64) << 32)
    }
}

#[cfg(test)]
static SYSCALLS: tests::support::MockSyscalls = tests::support::MockSyscalls;

#[cfg(not(test))]
static SYSCALLS: RealSyscalls = RealSyscalls;

#[derive(Debug)]
pub(super) struct SharedMemoryRegion {
    ptr: *mut u8,
    len: usize,
    #[cfg_attr(not(test), allow(dead_code))]
    fd: std::os::fd::OwnedFd,
}

impl SharedMemoryRegion {
    pub(super) fn create(size: usize, max_allocation_size: usize) -> Result<Self> {
        validate_size(size, max_allocation_size)?;

        let name = next_shm_name();
        let owned_fd = SYSCALLS.shm_open(&name)?;
        // Unlink immediately so the object is removed automatically after the last close.
        SYSCALLS.shm_unlink(&name)?;
        // New POSIX shared-memory objects start at size 0; set the requested length before mmap.
        SYSCALLS.ftruncate(&owned_fd, size)?;
        let raw_ptr = SYSCALLS.mmap(&owned_fd, size)?;

        Ok(Self {
            ptr: raw_ptr.cast::<u8>(),
            len: size,
            fd: owned_fd,
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn from_handle(
        size: usize,
        max_allocation_size: usize,
        handle: InterprocessMemoryHandle,
    ) -> Result<Self> {
        validate_size(size, max_allocation_size)?;
        let owned_mapping = match handle {
            InterprocessMemoryHandle::CpuSharedFd(raw) => raw,
            InterprocessMemoryHandle::GpuOpaqueFd(_) => {
                return Err(LavaFlowError::UnsupportedInterprocessHandle {
                    kind: "GpuOpaqueFd",
                });
            }
        };

        let raw_ptr = SYSCALLS.mmap(&owned_mapping, size)?;

        Ok(Self {
            ptr: raw_ptr.cast::<u8>(),
            len: size,
            fd: owned_mapping,
        })
    }

    pub(super) fn as_ptr(&self) -> *const u8 {
        self.ptr.cast_const()
    }

    pub(super) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    pub(super) fn size(&self) -> usize {
        self.len
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn export_handle(&self) -> Result<InterprocessMemoryHandle> {
        let owned_fd = SYSCALLS.dup_fd_cloexec(self.fd.as_raw_fd())?;
        Ok(InterprocessMemoryHandle::from_cpu_shared_fd(owned_fd))
    }
}

fn next_shm_name() -> CString {
    use std::sync::atomic::{AtomicU64, Ordering};

    static SHM_COUNTER: AtomicU64 = AtomicU64::new(1);

    let counter = SHM_COUNTER.fetch_add(1, Ordering::Relaxed);
    let random = SYSCALLS.random_u64();
    let name = format!(
        "/lava-flow-{}-{}-{:016x}",
        std::process::id(),
        counter,
        random
    );
    CString::new(name).expect("generated shared-memory name must not contain NUL")
}

impl Drop for SharedMemoryRegion {
    fn drop(&mut self) {
        if self.ptr.is_null() || self.len == 0 {
            return;
        }
        let _ = SYSCALLS.munmap(self.ptr.cast::<libc::c_void>(), self.len);
        self.ptr = std::ptr::null_mut();
        self.len = 0;
    }
}

struct RealSyscalls;

impl Syscalls for RealSyscalls {
    fn shm_open(&self, name: &CString) -> Result<OwnedFd> {
        let fd = unsafe {
            libc::shm_open(
                name.as_ptr(),
                // O_CREAT: create if absent, O_EXCL: fail if already present, O_RDWR: map read/write,
                // O_CLOEXEC: prevent fd inheritance across exec so child processes cannot retain access.
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR | libc::O_CLOEXEC,
                // S_IRUSR/S_IWUSR: owner read/write permissions.
                libc::S_IRUSR | libc::S_IWUSR,
            )
        };
        if fd < 0 {
            return Err(shared_memory_error("shm_open"));
        }
        let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(owned_fd)
    }

    fn shm_unlink(&self, name: &CString) -> Result<()> {
        let rc = unsafe { libc::shm_unlink(name.as_ptr()) };
        if rc != 0 {
            return Err(shared_memory_error("shm_unlink"));
        }
        Ok(())
    }

    fn ftruncate(&self, fd: &OwnedFd, size: usize) -> Result<()> {
        if size as u128 > libc::off_t::MAX as u128 {
            return Err(shared_memory_invalid_input(
                "ftruncate_size_overflow",
                "allocation size exceeds off_t range",
            ));
        }
        let rc = unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) };
        if rc != 0 {
            return Err(shared_memory_error("ftruncate"));
        }
        Ok(())
    }

    fn mmap(&self, fd: &OwnedFd, size: usize) -> Result<*mut libc::c_void> {
        let raw_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if raw_ptr == libc::MAP_FAILED {
            return Err(shared_memory_error("mmap"));
        }
        Ok(raw_ptr)
    }

    fn munmap(&self, addr: *mut libc::c_void, len: usize) -> Result<()> {
        let rc = unsafe { libc::munmap(addr, len) };
        if rc != 0 {
            return Err(shared_memory_error("munmap"));
        }
        Ok(())
    }

    fn fcntl_dupfd_cloexec(&self, fd: libc::c_int) -> Result<libc::c_int> {
        let duplicated_fd = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicated_fd < 0 {
            return Err(shared_memory_error("dup"));
        }
        Ok(duplicated_fd)
    }

    fn dup(&self, fd: libc::c_int) -> Result<libc::c_int> {
        let duplicated_fd = unsafe { libc::dup(fd) };
        if duplicated_fd < 0 {
            return Err(shared_memory_error("dup"));
        }
        Ok(duplicated_fd)
    }

    fn fcntl_setfd_cloexec(&self, fd: libc::c_int) -> Result<()> {
        let rc = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        if rc != 0 {
            return Err(shared_memory_error("fcntl_cloexec"));
        }
        Ok(())
    }

    fn getrandom(&self, buf: *mut libc::c_void, len: usize, flags: libc::c_uint) -> Result<usize> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let rc = unsafe { libc::getrandom(buf, len, flags) };
            if rc < 0 {
                return Err(shared_memory_error("getrandom"));
            }
            Ok(rc as usize)
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            let _ = (buf, len, flags);
            Err(shared_memory_error("getrandom"))
        }
    }

    fn open(&self, path: *const libc::c_char, flags: libc::c_int) -> Result<OwnedFd> {
        let fd = unsafe { libc::open(path, flags) };
        if fd < 0 {
            return Err(shared_memory_error("open"));
        }
        let owned_fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(owned_fd)
    }

    fn read(&self, fd: libc::c_int, buf: *mut libc::c_void, count: usize) -> Result<usize> {
        let rc = unsafe { libc::read(fd, buf, count) };
        if rc < 0 {
            return Err(shared_memory_error("read"));
        }
        Ok(rc as usize)
    }
}

fn shared_memory_invalid_input(operation: &'static str, message: &'static str) -> LavaFlowError {
    LavaFlowError::SharedMemoryOperation {
        operation,
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUFFER_SIZE: usize = 64;
    const TEST_BYTE_OFFSET: usize = 7;
    const TEST_BYTE_VALUE: u8 = 0x3c;

    fn run_with_fail<T>(op: &'static str, f: impl FnOnce() -> T) -> T {
        support::set_fail_ops(&[op]);
        f()
    }

    fn run_with_fail_ops<T>(ops: &[&'static str], f: impl FnOnce() -> T) -> T {
        support::set_fail_ops(ops);
        f()
    }

    fn allocate_standard_for_test(size: usize) -> Result<MemoryBuffer> {
        Allocator::with_max_allocation_size(usize::MAX).allocate(size)
    }

    fn dup_stdout_fd_for_test() -> OwnedFd {
        let dup_fd = unsafe { libc::dup(1) };
        assert!(dup_fd >= 0, "dup stdout for test");
        unsafe { OwnedFd::from_raw_fd(dup_fd) }
    }

    fn gpu_external_fd_for_test() -> OwnedFd {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "create pipe for gpu handle");
        let _read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        unsafe { OwnedFd::from_raw_fd(fds[1]) }
    }

    #[test]
    fn open_shared_region_round_trip_shares_bytes() {
        let mut buffer = allocate_standard_for_test(BUFFER_SIZE).expect("allocate buffer");
        buffer.as_mut_slice()[TEST_BYTE_OFFSET] = TEST_BYTE_VALUE;

        let handle = buffer.shared_handle().expect("export handle");
        let imported =
            SharedMemoryRegion::from_handle(BUFFER_SIZE, hard_max_cpu_allocation_size(), handle)
                .expect("import handle");

        unsafe {
            assert_eq!(*imported.as_ptr().add(TEST_BYTE_OFFSET), TEST_BYTE_VALUE);
        }
    }

    #[test]
    fn open_shared_region_rejects_gpu_handle() {
        let gpu_handle = InterprocessMemoryHandle::from_gpu_external_fd(gpu_external_fd_for_test());
        let err = SharedMemoryRegion::from_handle(
            BUFFER_SIZE,
            hard_max_cpu_allocation_size(),
            gpu_handle,
        )
        .expect_err("gpu handle must be rejected");
        assert!(matches!(
            err,
            LavaFlowError::UnsupportedInterprocessHandle {
                kind: "GpuOpaqueFd"
            }
        ));
    }

    #[test]
    fn dup_fd_fallback_path_succeeds() {
        let buffer = allocate_standard_for_test(BUFFER_SIZE).expect("allocate buffer");
        let handle = run_with_fail("dup_force_fallback", || buffer.shared_handle())
            .expect("dup fallback should succeed");
        assert!(handle.is_valid());
    }

    #[test]
    fn random_u64_falls_back_to_urandom_when_getrandom_fails() {
        let value = run_with_fail("getrandom", || SYSCALLS.random_u64());
        assert_ne!(value, 0);
    }

    #[test]
    fn random_u64_falls_back_to_time_when_entropy_sources_fail() {
        let value = run_with_fail_ops(&["getrandom", "open_urandom"], || SYSCALLS.random_u64());
        assert_ne!(value, 0);
    }

    #[test]
    fn random_u64_falls_back_to_time_when_urandom_read_fails() {
        let value = run_with_fail_ops(&["getrandom", "read_urandom"], || SYSCALLS.random_u64());
        assert_ne!(value, 0);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn shared_memory_error_uses_generic_error_when_errno_is_zero() {
        unsafe {
            *libc::__errno_location() = 0;
        }
        let err = shared_memory_error("unit_test_errno_zero");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation { source, .. }
                if source.kind() == std::io::ErrorKind::Other
        ));
    }

    #[test]
    fn ftruncate_rejects_off_t_overflow_size() {
        let dup_fd = dup_stdout_fd_for_test();
        let err = SYSCALLS
            .ftruncate(&dup_fd, usize::MAX)
            .expect_err("overflow must fail");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "ftruncate_size_overflow",
                ..
            }
        ));
    }

    #[test]
    fn failpoint_shm_open_error() {
        let err = run_with_fail("shm_open", || allocate_standard_for_test(BUFFER_SIZE))
            .expect_err("forced shm_open failure");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "shm_open",
                ..
            }
        ));
    }

    #[test]
    fn failpoint_shm_unlink_error() {
        let err = run_with_fail("shm_unlink", || allocate_standard_for_test(BUFFER_SIZE))
            .expect_err("forced shm_unlink failure");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "shm_unlink",
                ..
            }
        ));
    }

    #[test]
    fn failpoint_ftruncate_error() {
        let err = run_with_fail("ftruncate", || allocate_standard_for_test(BUFFER_SIZE))
            .expect_err("forced ftruncate failure");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "ftruncate",
                ..
            }
        ));
    }

    #[test]
    fn failpoint_mmap_error() {
        let err = run_with_fail("mmap", || allocate_standard_for_test(BUFFER_SIZE))
            .expect_err("forced mmap failure");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "mmap",
                ..
            }
        ));
    }

    #[test]
    fn failpoint_dup_error() {
        let buffer = allocate_standard_for_test(BUFFER_SIZE).expect("allocate for dup failpoint");
        let err = run_with_fail("dup", || buffer.shared_handle()).expect_err("forced dup failure");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "dup",
                ..
            }
        ));
    }

    #[test]
    fn drop_guard_returns_for_null_pointer() {
        let fd = dup_stdout_fd_for_test();
        let region = SharedMemoryRegion {
            ptr: std::ptr::null_mut(),
            len: 1,
            fd,
        };
        drop(region);
    }

    #[test]
    fn shm_open_syscall_error_path() {
        let name = std::ffi::CString::new("").expect("valid c string");
        let err = SYSCALLS.shm_open(&name).expect_err("shm_open should fail");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "shm_open",
                ..
            }
        ));
    }

    #[test]
    fn shm_unlink_syscall_error_path() {
        use std::ffi::CString;
        let name = CString::new("/lava-flow-nonexistent-unlink").expect("valid c string");
        let err = SYSCALLS.shm_unlink(&name).expect_err("unlink should fail");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "shm_unlink",
                ..
            }
        ));
    }

    #[test]
    fn ftruncate_syscall_error_path() {
        let dup_fd = dup_stdout_fd_for_test();
        let err = SYSCALLS
            .ftruncate(&dup_fd, BUFFER_SIZE)
            .expect_err("ftruncate should fail for stdout fd");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "ftruncate",
                ..
            }
        ));
    }

    #[test]
    fn mmap_syscall_error_path() {
        let dup_fd = dup_stdout_fd_for_test();
        let err = SYSCALLS
            .mmap(&dup_fd, BUFFER_SIZE)
            .expect_err("mmap should fail for stdout fd");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "mmap",
                ..
            }
        ));
    }

    #[test]
    fn munmap_syscall_error_path() {
        let err = RealSyscalls
            .munmap(std::ptr::null_mut(), 0)
            .expect_err("munmap should fail for zero length");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "munmap",
                ..
            }
        ));
    }

    #[test]
    fn fcntl_setfd_cloexec_syscall_error_path() {
        let err = RealSyscalls
            .fcntl_setfd_cloexec(-1)
            .expect_err("fcntl_setfd should fail for invalid fd");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "fcntl_cloexec",
                ..
            }
        ));
    }

    #[test]
    fn getrandom_syscall_error_path() {
        let err = RealSyscalls
            .getrandom(std::ptr::null_mut(), BUFFER_SIZE, 0)
            .expect_err("getrandom should fail for null buffer");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "getrandom",
                ..
            }
        ));
    }

    #[test]
    fn open_syscall_error_path() {
        let missing = b"/definitely/not/found/lava-flow\0";
        let err = RealSyscalls
            .open(missing.as_ptr().cast::<libc::c_char>(), libc::O_RDONLY)
            .expect_err("open should fail for missing file");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "open",
                ..
            }
        ));
    }

    #[test]
    fn read_syscall_error_path() {
        let err = RealSyscalls
            .read(-1, std::ptr::null_mut(), BUFFER_SIZE)
            .expect_err("read should fail for invalid fd");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "read",
                ..
            }
        ));
    }

    #[test]
    fn dup_fd_syscall_error_path() {
        let err = SYSCALLS
            .dup_fd_cloexec(-1)
            .expect_err("dup should fail for invalid fd");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "dup",
                ..
            }
        ));
    }

    #[test]
    fn dup_fd_fallback_setfd_failure_is_reported() {
        let buffer = allocate_standard_for_test(BUFFER_SIZE).expect("allocate buffer");
        let err = run_with_fail_ops(&["dup_force_fallback", "dup_setfd"], || {
            buffer.shared_handle()
        })
        .expect_err("dup fallback setfd should fail");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "fcntl_cloexec",
                ..
            }
        ));
    }
    pub(in crate::memory::cpu::unix) mod support {
        use super::*;

        thread_local! {
            static FAIL_OP_UNIX: std::cell::RefCell<Vec<&'static str>> = const { std::cell::RefCell::new(Vec::new()) };
        }

        pub fn set_fail_ops(ops: &[&'static str]) {
            FAIL_OP_UNIX.with(|cell| {
                let mut current = cell.borrow_mut();
                current.clear();
                current.extend_from_slice(ops);
            });
        }

        fn should_fail(op: &'static str) -> bool {
            FAIL_OP_UNIX.with(|cell| {
                let mut current = cell.borrow_mut();
                if current.first().copied() == Some(op) {
                    current.remove(0);
                    true
                } else {
                    false
                }
            })
        }

        fn has_fail(op: &'static str) -> bool {
            FAIL_OP_UNIX.with(|cell| cell.borrow().first().copied() == Some(op))
        }

        pub struct MockSyscalls;

        impl Syscalls for MockSyscalls {
            fn shm_open(&self, name: &CString) -> Result<OwnedFd> {
                if should_fail("shm_open") {
                    Err(shared_memory_error("shm_open"))
                } else {
                    RealSyscalls.shm_open(name)
                }
            }

            fn shm_unlink(&self, name: &CString) -> Result<()> {
                if should_fail("shm_unlink") {
                    Err(shared_memory_error("shm_unlink"))
                } else {
                    RealSyscalls.shm_unlink(name)
                }
            }

            fn ftruncate(&self, fd: &OwnedFd, size: usize) -> Result<()> {
                if should_fail("ftruncate") {
                    Err(shared_memory_error("ftruncate"))
                } else {
                    RealSyscalls.ftruncate(fd, size)
                }
            }

            fn mmap(&self, fd: &OwnedFd, size: usize) -> Result<*mut libc::c_void> {
                if should_fail("mmap") {
                    Err(shared_memory_error("mmap"))
                } else {
                    RealSyscalls.mmap(fd, size)
                }
            }

            fn munmap(&self, addr: *mut libc::c_void, len: usize) -> Result<()> {
                RealSyscalls.munmap(addr, len)
            }

            fn fcntl_dupfd_cloexec(&self, fd: libc::c_int) -> Result<libc::c_int> {
                if has_fail("dup") || should_fail("dup_force_fallback") {
                    Err(shared_memory_error("dup"))
                } else {
                    RealSyscalls.fcntl_dupfd_cloexec(fd)
                }
            }

            fn dup(&self, fd: libc::c_int) -> Result<libc::c_int> {
                if should_fail("dup") {
                    Err(shared_memory_error("dup"))
                } else {
                    RealSyscalls.dup(fd)
                }
            }

            fn fcntl_setfd_cloexec(&self, fd: libc::c_int) -> Result<()> {
                if should_fail("dup_setfd") {
                    Err(shared_memory_error("fcntl_cloexec"))
                } else {
                    RealSyscalls.fcntl_setfd_cloexec(fd)
                }
            }

            fn getrandom(
                &self,
                buf: *mut libc::c_void,
                len: usize,
                flags: libc::c_uint,
            ) -> Result<usize> {
                if should_fail("getrandom") {
                    Err(shared_memory_error("getrandom"))
                } else {
                    RealSyscalls.getrandom(buf, len, flags)
                }
            }

            fn open(&self, path: *const libc::c_char, flags: libc::c_int) -> Result<OwnedFd> {
                if should_fail("open_urandom") {
                    Err(shared_memory_error("open"))
                } else {
                    RealSyscalls.open(path, flags)
                }
            }

            fn read(&self, fd: libc::c_int, buf: *mut libc::c_void, count: usize) -> Result<usize> {
                if should_fail("read_urandom") {
                    Err(shared_memory_error("read"))
                } else {
                    RealSyscalls.read(fd, buf, count)
                }
            }
        }
    }
}
