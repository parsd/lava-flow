use super::InterprocessMemoryHandle;
use std::os::windows::io::OwnedHandle;

impl InterprocessMemoryHandle {
    pub(crate) fn from_gpu_external_handle(handle: OwnedHandle) -> Self {
        Self::GpuOpaqueWin32Handle(handle)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::FromRawHandle;

    #[test]
    fn from_gpu_external_handle_returns_valid_handle() {
        use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let current = unsafe { GetCurrentProcess() };
        let mut duplicated = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                current,
                current,
                current,
                &mut duplicated,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        assert_ne!(ok, 0, "duplicate current process handle");
        let owned = unsafe { OwnedHandle::from_raw_handle(duplicated) };
        let handle = InterprocessMemoryHandle::from_gpu_external_handle(owned);
        assert!(handle.is_valid());
        assert!(matches!(
            handle,
            InterprocessMemoryHandle::GpuOpaqueWin32Handle(_)
        ));
    }
}
