use super::InterprocessMemoryHandle;
use crate::error::{LavaFlowError, Result};
use std::os::windows::io::{FromRawHandle, OwnedHandle};

impl InterprocessMemoryHandle {
    pub(crate) fn from_gpu_id(_id: u64) -> Result<Self> {
        #[cfg(test)]
        if tests::take_fail_dup_gpu_handle() {
            return Err(LavaFlowError::SharedMemoryOperation {
                operation: "DuplicateHandle",
                source: std::io::Error::last_os_error(),
            });
        }

        let owned = duplicate_handle_same_access_current_process()?;
        Ok(Self::GpuOpaqueWin32Handle(owned))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_cpu_shared_handle(handle: OwnedHandle) -> Self {
        Self::CpuSharedWin32Handle(handle)
    }

    /// Returns whether the wrapped handle value is non-null/usable.
    #[cfg(test)]
    pub(crate) fn is_valid(&self) -> bool {
        use std::os::windows::io::AsRawHandle;
        // Windows APIs commonly use 0/NULL and INVALID_HANDLE_VALUE for invalid handles.
        match self {
            Self::GpuOpaqueWin32Handle(raw) | Self::CpuSharedWin32Handle(raw) => {
                let raw = raw.as_raw_handle();
                !raw.is_null() && raw as isize != -1
            }
        }
    }
}

fn duplicate_handle_same_access_current_process() -> Result<OwnedHandle> {
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let current_process = unsafe { GetCurrentProcess() };
    duplicate_handle_same_access(current_process, current_process, current_process)
}

fn duplicate_handle_same_access(
    source_process: windows_sys::Win32::Foundation::HANDLE,
    source: windows_sys::Win32::Foundation::HANDLE,
    target_process: windows_sys::Win32::Foundation::HANDLE,
) -> Result<OwnedHandle> {
    use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
    let mut duplicated = std::ptr::null_mut();
    let ok = unsafe {
        DuplicateHandle(
            source_process,
            source,
            target_process,
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
    Ok(owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, thread_local};

    thread_local! {
        static FAIL_DUP_GPU_HANDLE: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn take_fail_dup_gpu_handle() -> bool {
        FAIL_DUP_GPU_HANDLE.with(|flag| flag.replace(false))
    }

    #[test]
    fn from_gpu_id_returns_valid_handle() {
        let handle = InterprocessMemoryHandle::from_gpu_id(1).expect("create gpu handle");
        assert!(handle.is_valid());
    }

    #[test]
    fn from_gpu_id_forced_duplicate_failure_is_reported() {
        FAIL_DUP_GPU_HANDLE.with(|flag| flag.set(true));
        let err = InterprocessMemoryHandle::from_gpu_id(1).expect_err("forced failure");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "DuplicateHandle",
                ..
            }
        ));
    }

    #[test]
    fn duplicate_handle_same_access_reports_error_for_invalid_handles() {
        let err = duplicate_handle_same_access(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
        .expect_err("invalid handles must fail");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "DuplicateHandle",
                ..
            }
        ));
    }
}
