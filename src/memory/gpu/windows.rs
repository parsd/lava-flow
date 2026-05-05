use super::{
    DeviceContext, Result, VULKAN_API, VulkanApi, unsupported_interprocess_handle,
    vulkan_operation_error,
};
use crate::memory::allocator::InterprocessMemoryHandle;
use ash::vk;
use std::os::windows::io::{
    AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, IntoRawHandle, OwnedHandle, RawHandle,
};

pub(super) const EXTERNAL_MEMORY_HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32;

pub(super) struct ExternalMemoryDevice(ash::khr::external_memory_win32::Device);

impl std::fmt::Debug for ExternalMemoryDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalMemoryDevice")
            .finish_non_exhaustive()
    }
}

impl ExternalMemoryDevice {
    pub(super) fn new(instance: &ash::Instance, device: &ash::Device) -> Self {
        Self(ash::khr::external_memory_win32::Device::new(
            instance, device,
        ))
    }

    pub(super) fn required_extensions() -> &'static [*const i8] {
        const REQUIRED_EXTENSIONS: [*const i8; 1] =
            [ash::khr::external_memory_win32::NAME.as_ptr()];
        &REQUIRED_EXTENSIONS
    }

    pub(super) unsafe fn get_memory_win32_handle(
        &self,
        info: &vk::MemoryGetWin32HandleInfoKHR<'_>,
    ) -> std::result::Result<vk::HANDLE, vk::Result> {
        unsafe { self.0.get_memory_win32_handle(info) }
    }
}

/// Owned Windows Vulkan external-memory handle.
///
/// Handles returned by [`crate::memory::gpu::MemoryBuffer::external_handle`] are duplicates owned
/// by the caller. Vulkan clients can import them with
/// [`crate::memory::gpu::EXTERNAL_MEMORY_HANDLE_TYPE`] on the same logical GPU device.
#[derive(Debug)]
pub struct ExternalHandle(OwnedHandle);

impl ExternalHandle {
    /// Returns a borrowed Win32 handle for Vulkan import calls.
    pub fn as_handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }

    pub(super) fn from_interprocess_handle(handle: InterprocessMemoryHandle) -> Result<Self> {
        match handle {
            InterprocessMemoryHandle::GpuOpaqueWin32Handle(handle) => Ok(Self(handle)),
            InterprocessMemoryHandle::CpuSharedWin32Handle(_) => {
                Err(unsupported_interprocess_handle("CpuSharedWin32Handle"))
            }
        }
    }

    pub(super) fn duplicate_for_ipc(&self) -> Result<InterprocessMemoryHandle> {
        Ok(InterprocessMemoryHandle::from_gpu_external_handle(
            self.duplicate_owned()?,
        ))
    }

    pub(super) fn try_clone(&self) -> Result<Self> {
        Ok(Self(self.duplicate_owned()?))
    }

    fn duplicate_owned(&self) -> Result<OwnedHandle> {
        self.0
            .as_handle()
            .try_clone_to_owned()
            .map_err(|err| vulkan_operation_error("dup_external_handle", err.to_string()))
    }
}

impl AsHandle for ExternalHandle {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }
}

impl AsRawHandle for ExternalHandle {
    fn as_raw_handle(&self) -> RawHandle {
        self.0.as_raw_handle()
    }
}

impl IntoRawHandle for ExternalHandle {
    fn into_raw_handle(self) -> RawHandle {
        self.0.into_raw_handle()
    }
}

impl From<ExternalHandle> for OwnedHandle {
    fn from(handle: ExternalHandle) -> Self {
        handle.0
    }
}

impl DeviceContext {
    pub(super) fn export_memory_handle(&self, memory: vk::DeviceMemory) -> Result<ExternalHandle> {
        let info = vk::MemoryGetWin32HandleInfoKHR::default()
            .memory(memory)
            .handle_type(EXTERNAL_MEMORY_HANDLE_TYPE);
        let get_handle_result =
            unsafe { self.external_memory_device.get_memory_win32_handle(&info) };
        let raw_handle = get_handle_result
            .map_err(|err| vulkan_operation_error("get_memory_win32_handle", err.to_string()))?;
        let owned = unsafe { OwnedHandle::from_raw_handle(raw_handle as *mut std::ffi::c_void) };
        Ok(ExternalHandle(owned))
    }

    pub(super) fn import_memory_handle(
        &self,
        buffer: vk::Buffer,
        allocation_size: u64,
        memory_type_index: u32,
        handle: InterprocessMemoryHandle,
    ) -> Result<vk::DeviceMemory> {
        let handle = match handle {
            InterprocessMemoryHandle::GpuOpaqueWin32Handle(handle) => handle,
            InterprocessMemoryHandle::CpuSharedWin32Handle(_) => {
                return Err(unsupported_interprocess_handle("CpuSharedWin32Handle"));
            }
        };
        let mut import_memory_info = vk::ImportMemoryWin32HandleInfoKHR::default()
            .handle_type(EXTERNAL_MEMORY_HANDLE_TYPE)
            .handle(handle.as_raw_handle() as vk::HANDLE);
        let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_memory_info)
            .push_next(&mut dedicated_info);
        VULKAN_API.allocate_memory(&self.device, &alloc_info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LavaFlowError;
    use std::os::windows::io::{FromRawHandle, IntoRawHandle};

    const BUFFER_SIZE: usize = 64;

    fn duplicate_current_process_handle_for_test() -> OwnedHandle {
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
        unsafe { OwnedHandle::from_raw_handle(duplicated) }
    }

    fn cpu_shared_handle_for_test() -> InterprocessMemoryHandle {
        crate::memory::cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(BUFFER_SIZE)
            .expect("allocate cpu buffer")
            .shared_handle()
            .expect("export cpu shared handle")
    }

    #[test]
    fn required_extensions_expose_platform_external_memory_extension() {
        let extensions = ExternalMemoryDevice::required_extensions();

        assert_eq!(extensions.len(), 1);
        let extension = unsafe { std::ffi::CStr::from_ptr(extensions[0]) };
        assert_eq!(extension, ash::khr::external_memory_win32::NAME);
    }

    #[test]
    fn external_memory_handle_type_matches_platform() {
        assert!(EXTERNAL_MEMORY_HANDLE_TYPE == vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32);
        assert!(
            EXTERNAL_MEMORY_HANDLE_TYPE.contains(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32)
        );
    }

    #[test]
    fn from_interprocess_handle_accepts_gpu_handle_directly() {
        let handle = InterprocessMemoryHandle::from_gpu_external_handle(
            duplicate_current_process_handle_for_test(),
        );
        let external = ExternalHandle::from_interprocess_handle(handle).expect("gpu handle");

        assert!(!external.as_handle().as_raw_handle().is_null());
    }

    #[test]
    fn external_handle_duplicates_handle_without_vulkan_context() {
        let handle = InterprocessMemoryHandle::from_gpu_external_handle(
            duplicate_current_process_handle_for_test(),
        );
        let external = ExternalHandle::from_interprocess_handle(handle).expect("gpu handle");

        let cloned = external.try_clone().expect("clone external handle");
        assert!(!cloned.as_handle().as_raw_handle().is_null());

        let ipc_handle = external.duplicate_for_ipc().expect("duplicate for ipc");
        assert!(crate::memory::allocator::tests::support::handle_is_gpu(
            &ipc_handle
        ));
        let round_tripped =
            ExternalHandle::from_interprocess_handle(ipc_handle).expect("round-trip gpu handle");
        assert!(!round_tripped.as_handle().as_raw_handle().is_null());
    }

    #[test]
    fn external_handle_debug_uses_type_name() {
        let handle = InterprocessMemoryHandle::from_gpu_external_handle(
            duplicate_current_process_handle_for_test(),
        );
        let external = ExternalHandle::from_interprocess_handle(handle).expect("gpu handle");

        assert!(format!("{external:?}").contains("ExternalHandle"));
    }

    #[test]
    fn from_interprocess_handle_rejects_cpu_handle_directly() {
        let err = ExternalHandle::from_interprocess_handle(cpu_shared_handle_for_test())
            .expect_err("cpu handle must be rejected");

        assert!(matches!(
            err,
            LavaFlowError::UnsupportedInterprocessHandle {
                kind: "CpuSharedWin32Handle",
            }
        ));
    }

    #[test]
    fn export_memory_handle_and_duplicate_for_ipc_directly() {
        if let Ok(allocator) = super::super::Allocator::new() {
            let buffer = allocator
                .allocate(BUFFER_SIZE)
                .expect("allocate gpu buffer");
            let external = allocator
                .context
                .export_memory_handle(buffer.memory)
                .expect("export external handle");
            let handle = external.duplicate_for_ipc().expect("duplicate for ipc");
            assert!(crate::memory::allocator::tests::support::handle_is_gpu(
                &handle
            ));
        }
    }

    #[test]
    fn external_handle_supports_public_handle_accessors_and_conversions() {
        if let Ok(allocator) = super::super::Allocator::new() {
            let buffer = allocator
                .allocate(BUFFER_SIZE)
                .expect("allocate gpu buffer");
            let external = buffer.external_handle().expect("export external handle");
            assert!(!external.as_handle().as_raw_handle().is_null());
            assert!(!AsHandle::as_handle(&external).as_raw_handle().is_null());
            assert!(!AsRawHandle::as_raw_handle(&external).is_null());

            let cloned = external.try_clone().expect("clone external handle");
            let owned = OwnedHandle::from(cloned);
            assert!(!owned.as_raw_handle().is_null());

            let raw = IntoRawHandle::into_raw_handle(external);
            let _owned_again = unsafe { OwnedHandle::from_raw_handle(raw) };
        }
    }

    #[test]
    fn import_memory_handle_rejects_cpu_handle_before_vulkan_import() {
        if let Ok(allocator) = super::super::Allocator::new() {
            let err = allocator
                .context
                .import_memory_handle(
                    vk::Buffer::null(),
                    BUFFER_SIZE as u64,
                    0,
                    cpu_shared_handle_for_test(),
                )
                .expect_err("cpu handle must not be accepted by gpu import");

            assert!(matches!(
                err,
                LavaFlowError::UnsupportedInterprocessHandle {
                    kind: "CpuSharedWin32Handle",
                }
            ));
        }
    }

    #[test]
    fn from_shared_handle_imports_exported_gpu_handle() {
        if let Ok(allocator) = super::super::Allocator::new() {
            let buffer = allocator
                .allocate(BUFFER_SIZE)
                .expect("allocate gpu buffer");
            let handle = buffer.shared_handle().expect("export handle");
            let imported = super::super::MemoryBuffer::from_shared_handle(
                allocator.device_id(),
                BUFFER_SIZE,
                handle,
            )
            .expect("import gpu handle");

            assert_eq!(imported.device_id(), allocator.device_id());
            assert_eq!(imported.size(), BUFFER_SIZE);
            assert!(imported.allocation_size() >= BUFFER_SIZE as u64);
            let reexported = imported.shared_handle().expect("re-export imported handle");
            assert!(crate::memory::allocator::tests::support::handle_is_gpu(
                &reexported
            ));
        }
    }

    #[test]
    fn from_shared_handle_rejects_cpu_handle_before_backend_creation() {
        let err = super::super::MemoryBuffer::from_shared_handle(
            super::super::DEFAULT_DEVICE_ID,
            BUFFER_SIZE,
            cpu_shared_handle_for_test(),
        )
        .expect_err("cpu handle must not be accepted as gpu import");

        assert!(matches!(
            err,
            LavaFlowError::UnsupportedInterprocessHandle {
                kind: "CpuSharedWin32Handle",
            }
        ));
    }
}
