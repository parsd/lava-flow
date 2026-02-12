use crate::error::{AllocationReason, LavaFlowError, Result};
use crate::memory::allocator::InterprocessMemoryHandle;

/// CPU-side allocation strategy.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CpuAllocationStrategy {
    /// Default host allocation.
    Standard,
    /// Host memory intended for GPU staging.
    GpuPinned,
}

/// CPU-backed memory buffer metadata and storage.
#[derive(Debug)]
pub struct CpuMemoryBuffer {
    bytes: Vec<u8>,
    strategy: CpuAllocationStrategy,
    gpu_pinned: bool,
    shared_handle: InterprocessMemoryHandle,
}

impl CpuMemoryBuffer {
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

    /// Returns the allocation strategy used to create this buffer.
    pub fn strategy(&self) -> CpuAllocationStrategy {
        self.strategy
    }

    /// Returns whether this allocation is pinned for GPU staging access.
    pub fn gpu_pinned(&self) -> bool {
        self.gpu_pinned
    }

    /// Returns alignment guaranteed by the current `Vec<u8>` backing storage.
    pub fn alignment(&self) -> usize {
        std::mem::align_of_val(self.bytes.as_slice())
    }

    /// Returns the interprocess shared-memory handle for this CPU buffer.
    pub fn shared_handle(&self) -> InterprocessMemoryHandle {
        self.shared_handle
    }
}

/// CPU allocation backend.
#[derive(Default, Debug)]
pub struct CpuAllocator {
    next_shared_handle_id: u64,
}

impl CpuAllocator {
    /// Creates a CPU allocator backend.
    pub fn new() -> Self {
        Self {
            next_shared_handle_id: 1,
        }
    }

    /// Allocates standard host memory.
    pub fn allocate_standard(&mut self, size: usize) -> Result<CpuMemoryBuffer> {
        let bytes = allocate_bytes(size)?;
        let shared_handle = self.next_shared_handle();
        Ok(CpuMemoryBuffer {
            bytes,
            strategy: CpuAllocationStrategy::Standard,
            gpu_pinned: false,
            shared_handle,
        })
    }

    /// Allocates host memory intended for CPU<->GPU staging.
    pub fn allocate_gpu_pinned(&mut self, size: usize) -> Result<CpuMemoryBuffer> {
        let bytes = allocate_bytes(size)?;
        let shared_handle = self.next_shared_handle();
        Ok(CpuMemoryBuffer {
            bytes,
            strategy: CpuAllocationStrategy::GpuPinned,
            gpu_pinned: true,
            shared_handle,
        })
    }

    fn next_shared_handle(&mut self) -> InterprocessMemoryHandle {
        let handle = InterprocessMemoryHandle::from_cpu_shared_id(self.next_shared_handle_id);
        self.next_shared_handle_id = self
            .next_shared_handle_id
            .checked_add(1)
            .unwrap_or(self.next_shared_handle_id);
        handle
    }
}

fn allocate_bytes(size: usize) -> Result<Vec<u8>> {
    if size == 0 {
        return Err(LavaFlowError::InvalidAllocationRequest {
            size,
            reason: AllocationReason::ZeroSize,
        });
    }
    Ok(vec![0; size])
}
