use crate::error::{AllocationReason, LavaFlowError, Result};
use crate::memory::allocator::InterprocessMemoryHandle;

/// GPU-backed memory buffer metadata and storage.
#[derive(Debug)]
pub struct GpuMemoryBuffer {
    bytes: Vec<u8>,
    device_id: u32,
    #[cfg_attr(not(any(test, windows)), allow(dead_code))]
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
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn external_handle(&self) -> &InterprocessMemoryHandle {
        &self.external_handle
    }
}

/// Vulkan-oriented GPU allocator backend.
///
/// This initial phase-2 implementation models the API and metadata flow while
/// full Vulkan device memory integration is added incrementally.
#[derive(Debug)]
pub struct Allocator {
    available_device_ids: Vec<u32>,
    next_handle_id: u64,
}

impl Default for Allocator {
    fn default() -> Self {
        Self {
            available_device_ids: vec![0],
            next_handle_id: 1,
        }
    }
}

impl Allocator {
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

        let handle = InterprocessMemoryHandle::from_gpu_id(self.next_handle_id)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    const BUFFER_SIZE: usize = 64;
    const DEVICE_ID_0: u32 = 0;
    const UNKNOWN_DEVICE_ID: u32 = 99;

    #[test]
    fn allocate_returns_buffer_with_valid_handle() {
        let mut allocator = Allocator::new();
        let buffer = allocator
            .allocate(BUFFER_SIZE, DEVICE_ID_0)
            .expect("allocate gpu buffer");
        assert_eq!(buffer.size(), BUFFER_SIZE);
        assert_eq!(buffer.device_id(), DEVICE_ID_0);
        assert!(buffer.external_handle().is_valid());
        #[cfg(unix)]
        assert!(matches!(
            buffer.external_handle(),
            &InterprocessMemoryHandle::GpuOpaqueFd(_)
        ));
        #[cfg(windows)]
        assert!(matches!(
            buffer.external_handle(),
            &InterprocessMemoryHandle::GpuOpaqueWin32Handle(_)
        ));
    }

    #[test]
    fn allocate_rejects_zero_size() {
        let mut allocator = Allocator::new();
        let err = allocator
            .allocate(0, DEVICE_ID_0)
            .expect_err("zero-sized allocation must fail");
        assert!(matches!(
            err,
            LavaFlowError::InvalidAllocationRequest {
                size: 0,
                reason: AllocationReason::ZeroSize,
            }
        ));
    }

    #[test]
    fn allocate_rejects_unknown_device() {
        let mut allocator = Allocator::new();
        let err = allocator
            .allocate(BUFFER_SIZE, UNKNOWN_DEVICE_ID)
            .expect_err("unknown device must fail");
        assert!(matches!(
            err,
            LavaFlowError::GpuDeviceNotFound {
                device_id: UNKNOWN_DEVICE_ID
            }
        ));
    }

    #[test]
    fn has_device_reports_visible_device() {
        let allocator = Allocator::new();
        assert!(allocator.has_device(DEVICE_ID_0));
        assert!(!allocator.has_device(UNKNOWN_DEVICE_ID));
    }
}
