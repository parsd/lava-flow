use super::InterprocessMemoryHandle;
use crate::error::{LavaFlowError, Result};
use std::os::fd::{FromRawFd, OwnedFd};

impl InterprocessMemoryHandle {
    pub(crate) fn from_gpu_id(_id: u64) -> Result<Self> {
        let duplicated = unsafe { libc::dup(1) };
        if duplicated < 0 {
            return Err(LavaFlowError::SharedMemoryOperation {
                operation: "dup_gpu_handle",
                source: std::io::Error::last_os_error(),
            });
        }
        let owned = unsafe { OwnedFd::from_raw_fd(duplicated) };
        Ok(Self::GpuOpaqueFd(owned))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_cpu_shared_handle(fd: OwnedFd) -> Self {
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

    #[test]
    fn from_gpu_id_reports_dup_failure_when_stdout_fd_invalid() {
        let backup_stdout = unsafe { libc::dup(1) };
        assert!(backup_stdout >= 0, "dup stdout for backup");

        let close_rc = unsafe { libc::close(1) };
        assert_eq!(close_rc, 0, "close stdout to force dup failure");

        let err = InterprocessMemoryHandle::from_gpu_id(1).expect_err("dup should fail");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "dup_gpu_handle",
                ..
            }
        ));

        let restore_rc = unsafe { libc::dup2(backup_stdout, 1) };
        assert!(restore_rc >= 0, "restore stdout");
        let _ = unsafe { libc::close(backup_stdout) };

        let handle = InterprocessMemoryHandle::from_gpu_id(2).expect("dup succeeds after restore");
        assert!(handle.is_valid());
        assert!(matches!(handle, InterprocessMemoryHandle::GpuOpaqueFd(_)));
    }

    #[test]
    fn from_cpu_shared_handle_reports_valid_fd() {
        let duplicated = unsafe { libc::dup(1) };
        assert!(duplicated >= 0, "dup stdout for cpu handle");
        let owned = unsafe { OwnedFd::from_raw_fd(duplicated) };
        let handle = InterprocessMemoryHandle::from_cpu_shared_handle(owned);
        assert!(handle.is_valid());
        assert!(matches!(handle, InterprocessMemoryHandle::CpuSharedFd(_)));
    }
}
