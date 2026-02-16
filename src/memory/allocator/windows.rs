use super::InterprocessMemoryHandle;
use crate::error::{LavaFlowError, Result};
#[cfg(test)]
use std::os::windows::io::AsRawHandle;
use std::os::windows::io::{FromRawHandle, OwnedHandle};

impl InterprocessMemoryHandle {
    pub(crate) fn from_gpu_id(_id: u64) -> Result<Self> {
        use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let current_process = unsafe { GetCurrentProcess() };
        let mut duplicated = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                current_process,
                current_process,
                current_process,
                &mut duplicated,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(LavaFlowError::SharedMemoryOperation {
                operation: "DuplicateHandle",
                source: std::io::Error::last_os_error(),
            });
        }

        let owned = unsafe { OwnedHandle::from_raw_handle(duplicated) };
        Ok(Self::GpuOpaqueWin32Handle(owned))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_cpu_shared_handle(handle: OwnedHandle) -> Self {
        Self::CpuSharedWin32Handle(handle)
    }

    /// Returns whether the wrapped handle value is non-null/usable.
    #[cfg(test)]
    pub(crate) fn is_valid(&self) -> bool {
        // Windows APIs commonly use 0/NULL and INVALID_HANDLE_VALUE for invalid handles.
        match self {
            Self::GpuOpaqueWin32Handle(raw) | Self::CpuSharedWin32Handle(raw) => {
                let raw = raw.as_raw_handle();
                !raw.is_null() && raw as isize != -1
            }
        }
    }
}
