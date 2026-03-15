use super::InterprocessMemoryHandle;
use std::os::fd::OwnedFd;

impl InterprocessMemoryHandle {
    pub(crate) fn from_gpu_external_fd(fd: OwnedFd) -> Self {
        Self::GpuOpaqueFd(fd)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_cpu_shared_fd(fd: OwnedFd) -> Self {
        Self::CpuSharedFd(fd)
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
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;

    #[test]
    fn from_gpu_external_fd_reports_valid_fd() {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "create pipe for gpu handle");
        let _read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let owned = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        let handle = InterprocessMemoryHandle::from_gpu_external_fd(owned);
        assert!(handle.is_valid());
        assert!(matches!(handle, InterprocessMemoryHandle::GpuOpaqueFd(_)));
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
        assert!(matches!(handle, InterprocessMemoryHandle::CpuSharedFd(_)));
    }
}
