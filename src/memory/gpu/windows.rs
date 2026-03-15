use super::{DeviceContext, Result, vulkan_operation_error};
use crate::memory::allocator::InterprocessMemoryHandle;
use ash::vk;
use std::os::windows::io::{AsHandle, FromRawHandle, OwnedHandle};

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
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUFFER_SIZE: usize = 64;

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
}
