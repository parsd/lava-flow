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
