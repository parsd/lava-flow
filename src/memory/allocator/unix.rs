use super::InterprocessMemoryHandle;
use crate::error::LavaFlowError;
use std::os::fd::OwnedFd;

impl InterprocessMemoryHandle {
    pub(crate) fn from_gpu_external_fd(fd: OwnedFd) -> Self {
        Self::GpuOpaqueFd(fd)
    }

    pub(crate) fn from_cpu_shared_fd(fd: OwnedFd) -> Self {
        Self::CpuSharedFd(fd)
    }

    pub(crate) fn try_clone(&self) -> crate::error::Result<Self> {
        use std::os::fd::{AsFd, BorrowedFd};

        fn clone_fd(fd: BorrowedFd<'_>) -> crate::error::Result<OwnedFd> {
            fd.try_clone_to_owned()
                .map_err(|source| LavaFlowError::SharedMemoryOperation {
                    operation: "dup_shared_handle",
                    source,
                })
        }

        match self {
            Self::GpuOpaqueFd(fd) => Ok(Self::GpuOpaqueFd(clone_fd(fd.as_fd())?)),
            Self::CpuSharedFd(fd) => Ok(Self::CpuSharedFd(clone_fd(fd.as_fd())?)),
        }
    }

    /// Returns whether the wrapped handle value is non-null/usable.
    #[cfg(test)]
    pub(crate) fn is_valid(&self) -> bool {
        use std::os::fd::AsRawFd;
        match self {
            Self::GpuOpaqueFd(raw) | Self::CpuSharedFd(raw) => raw.as_raw_fd() >= 0,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::fd::FromRawFd;

    pub(crate) mod support {
        use crate::memory::allocator::InterprocessMemoryHandle;

        pub(crate) fn handle_is_cpu(handle: &InterprocessMemoryHandle) -> bool {
            matches!(handle, InterprocessMemoryHandle::CpuSharedFd(_))
        }

        pub(crate) fn handle_is_gpu(handle: &InterprocessMemoryHandle) -> bool {
            matches!(handle, InterprocessMemoryHandle::GpuOpaqueFd(_))
        }
    }

    #[test]
    fn from_gpu_external_fd_reports_valid_fd() {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "create pipe for gpu handle");
        let _read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let owned = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        let handle = InterprocessMemoryHandle::from_gpu_external_fd(owned);
        assert!(handle.is_valid());
        assert!(support::handle_is_gpu(&handle));
    }

    #[test]
    fn from_cpu_shared_fd_reports_valid_fd() {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "create pipe for cpu handle");
        let _read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let owned = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        let handle = InterprocessMemoryHandle::from_cpu_shared_fd(owned);
        assert!(handle.is_valid());
        assert!(support::handle_is_cpu(&handle));
    }

    #[test]
    fn gpu_external_fd_try_clone_preserves_gpu_variant() {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "create pipe for gpu handle");
        let _read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let owned = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        let handle = InterprocessMemoryHandle::from_gpu_external_fd(owned);

        let cloned = handle.try_clone().expect("clone gpu external handle");
        assert!(support::handle_is_gpu(&cloned));
        assert!(cloned.is_valid());
    }

    #[test]
    fn cpu_shared_fd_try_clone_preserves_cpu_variant() {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "create pipe for cpu handle");
        let _read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let owned = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        let handle = InterprocessMemoryHandle::from_cpu_shared_fd(owned);

        let cloned = handle.try_clone().expect("clone cpu shared handle");
        assert!(support::handle_is_cpu(&cloned));
        assert!(cloned.is_valid());
    }
}
