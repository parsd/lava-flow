use crate::error::{AllocationReason, LavaFlowError, Result};
use crate::memory::allocator::InterprocessMemoryHandle;

/// GPU-backed memory buffer metadata and storage.
#[derive(Debug)]
pub struct GpuMemoryBuffer {
    bytes: Vec<u8>,
    device_id: u32,
    external_handle: InterprocessMemoryHandle,
}

impl GpuMemoryBuffer {
    /// Returns an immutable raw pointer to the beginning of the buffer.
    pub fn as_ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
    }

    /// Returns a mutable raw pointer to the beginning of the buffer.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr()
    }

    /// Returns the buffer size in bytes.
    pub fn size(&self) -> usize {
        self.bytes.len()
    }

    /// Returns the device identifier used for allocation.
    pub fn device_id(&self) -> u32 {
        self.device_id
    }

    /// Returns the exportable external handle.
    pub fn external_handle(&self) -> InterprocessMemoryHandle {
        self.external_handle
    }
}

/// Vulkan-oriented GPU allocator backend.
///
/// This initial phase-2 implementation models the API and metadata flow while
/// full Vulkan device memory integration is added incrementally.
#[derive(Debug)]
pub struct VulkanAllocator {
    available_device_ids: Vec<u32>,
    next_handle_id: u64,
}

impl Default for VulkanAllocator {
    fn default() -> Self {
        Self {
            available_device_ids: vec![0],
            next_handle_id: 1,
        }
    }
}

impl VulkanAllocator {
    /// Probes whether Vulkan allocation should be enabled for this process.
    ///
    /// This phase-2 scaffold allows disabling the GPU backend via
    /// `LAVA_FLOW_DISABLE_VULKAN` to exercise CPU-only paths.
    pub fn probe() -> Option<Self> {
        if std::env::var_os("LAVA_FLOW_DISABLE_VULKAN").is_some() {
            None
        } else {
            Some(Self::new())
        }
    }

    /// Creates a GPU allocator backend with default visible device ids.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if the backend can allocate for the requested device id.
    pub fn has_device(&self, device_id: u32) -> bool {
        self.available_device_ids.contains(&device_id)
    }

    /// Allocates a GPU buffer and tags it with an exportable external handle.
    pub fn allocate(&mut self, size: usize, device_id: u32) -> Result<GpuMemoryBuffer> {
        if size == 0 {
            return Err(LavaFlowError::InvalidAllocationRequest {
                size,
                reason: AllocationReason::ZeroSize,
            });
        }
        if !self.has_device(device_id) {
            return Err(LavaFlowError::GpuDeviceNotFound { device_id });
        }

        let handle = InterprocessMemoryHandle::from_gpu_id(self.next_handle_id);
        self.next_handle_id = self
            .next_handle_id
            .checked_add(1)
            .unwrap_or(self.next_handle_id);

        Ok(GpuMemoryBuffer {
            bytes: vec![0; size],
            device_id,
            external_handle: handle,
        })
    }
}
