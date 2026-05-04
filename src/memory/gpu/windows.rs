use super::{
    DeviceContext, Result, VULKAN_API, VulkanApi, unsupported_interprocess_handle,
    vulkan_operation_error,
};
use crate::memory::allocator::InterprocessMemoryHandle;
use ash::vk;
use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};

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

#[derive(Debug)]
pub(super) struct ExternalHandle(OwnedHandle);

impl ExternalHandle {
    pub(super) fn from_interprocess_handle(handle: InterprocessMemoryHandle) -> Result<Self> {
        match handle {
            InterprocessMemoryHandle::GpuOpaqueWin32Handle(handle) => Ok(Self(handle)),
            InterprocessMemoryHandle::CpuSharedWin32Handle(_) => {
                Err(unsupported_interprocess_handle("CpuSharedWin32Handle"))
            }
        }
    }

    pub(super) fn duplicate_for_ipc(&self) -> Result<InterprocessMemoryHandle> {
        let duplicated = self
            .0
            .as_handle()
            .try_clone_to_owned()
            .map_err(|err| vulkan_operation_error("dup_external_handle", err.to_string()))?;
        Ok(InterprocessMemoryHandle::from_gpu_external_handle(
            duplicated,
        ))
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

    const BUFFER_SIZE: usize = 64;

    fn cpu_shared_handle_for_test() -> InterprocessMemoryHandle {
        crate::memory::cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(BUFFER_SIZE)
            .expect("allocate cpu buffer")
            .shared_handle()
            .expect("export cpu shared handle")
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
            assert!(matches!(
                handle,
                InterprocessMemoryHandle::GpuOpaqueWin32Handle(_)
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
            assert!(matches!(
                reexported,
                InterprocessMemoryHandle::GpuOpaqueWin32Handle(_)
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
