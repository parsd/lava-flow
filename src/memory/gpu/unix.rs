use super::{
    DeviceContext, Result, VULKAN_API, VulkanApi, unsupported_interprocess_handle,
    vulkan_operation_error,
};
use crate::memory::allocator::InterprocessMemoryHandle;
use ash::vk;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};

pub(super) const EXTERNAL_MEMORY_HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;

pub(super) struct ExternalMemoryDevice(ash::khr::external_memory_fd::Device);

impl std::fmt::Debug for ExternalMemoryDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExternalMemoryDevice")
            .finish_non_exhaustive()
    }
}

impl ExternalMemoryDevice {
    pub(super) fn new(instance: &ash::Instance, device: &ash::Device) -> Self {
        Self(ash::khr::external_memory_fd::Device::new(instance, device))
    }

    pub(super) fn required_extensions() -> &'static [*const i8] {
        const REQUIRED_EXTENSIONS: [*const i8; 1] = [ash::khr::external_memory_fd::NAME.as_ptr()];
        &REQUIRED_EXTENSIONS
    }

    pub(super) unsafe fn get_memory_fd(
        &self,
        info: &vk::MemoryGetFdInfoKHR<'_>,
    ) -> std::result::Result<i32, vk::Result> {
        unsafe { self.0.get_memory_fd(info) }
    }
}

/// Owned Unix Vulkan external-memory file descriptor.
///
/// Handles returned by [`crate::memory::gpu::MemoryBuffer::external_handle`] are duplicates owned
/// by the caller. Vulkan clients can import them with
/// [`crate::memory::gpu::EXTERNAL_MEMORY_HANDLE_TYPE`] on the same logical GPU device.
#[derive(Debug)]
pub struct ExternalHandle(OwnedFd);

impl ExternalHandle {
    /// Returns a borrowed file descriptor for Vulkan import calls.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }

    pub(super) fn from_interprocess_handle(handle: InterprocessMemoryHandle) -> Result<Self> {
        match handle {
            InterprocessMemoryHandle::GpuOpaqueFd(fd) => Ok(Self(fd)),
            InterprocessMemoryHandle::CpuSharedFd(_) => {
                Err(unsupported_interprocess_handle("CpuSharedFd"))
            }
        }
    }

    pub(super) fn duplicate_for_ipc(&self) -> Result<InterprocessMemoryHandle> {
        Ok(InterprocessMemoryHandle::from_gpu_external_fd(
            self.duplicate_owned()?,
        ))
    }

    pub(super) fn try_clone(&self) -> Result<Self> {
        Ok(Self(self.duplicate_owned()?))
    }

    fn duplicate_owned(&self) -> Result<OwnedFd> {
        self.0
            .as_fd()
            .try_clone_to_owned()
            .map_err(|err| vulkan_operation_error("dup_external_handle", err.to_string()))
    }
}

impl AsFd for ExternalHandle {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl AsRawFd for ExternalHandle {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl IntoRawFd for ExternalHandle {
    fn into_raw_fd(self) -> RawFd {
        self.0.into_raw_fd()
    }
}

impl From<ExternalHandle> for OwnedFd {
    fn from(handle: ExternalHandle) -> Self {
        handle.0
    }
}

impl DeviceContext {
    pub(super) fn export_memory_handle(&self, memory: vk::DeviceMemory) -> Result<ExternalHandle> {
        let info = vk::MemoryGetFdInfoKHR::default()
            .memory(memory)
            .handle_type(EXTERNAL_MEMORY_HANDLE_TYPE);
        let get_fd_result = unsafe { self.external_memory_device.get_memory_fd(&info) };
        let fd = get_fd_result
            .map_err(|err| vulkan_operation_error("get_memory_fd", err.to_string()))?;
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(ExternalHandle(owned))
    }

    pub(super) fn import_memory_handle(
        &self,
        buffer: vk::Buffer,
        allocation_size: u64,
        memory_type_index: u32,
        handle: InterprocessMemoryHandle,
    ) -> Result<vk::DeviceMemory> {
        let fd = match handle {
            InterprocessMemoryHandle::GpuOpaqueFd(fd) => fd,
            InterprocessMemoryHandle::CpuSharedFd(_) => {
                return Err(unsupported_interprocess_handle("CpuSharedFd"));
            }
        };
        let mut import_memory_info = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(EXTERNAL_MEMORY_HANDLE_TYPE)
            .fd(fd.as_raw_fd());
        let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut import_memory_info)
            .push_next(&mut dedicated_info);
        let memory = VULKAN_API.allocate_memory(&self.device, &alloc_info)?;
        // OPAQUE_FD import transfers fd ownership to Vulkan on successful import.
        let _ = fd.into_raw_fd();
        Ok(memory)
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
            assert!(crate::memory::allocator::tests::support::handle_is_gpu(
                &handle
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
                kind: "CpuSharedFd",
            }
        ));
    }
}
