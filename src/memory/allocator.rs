use crate::error::{LavaFlowError, Result};
use crate::memory::cpu::{CpuAllocationStrategy, CpuAllocator, CpuMemoryBuffer};
use crate::memory::gpu::{GpuMemoryBuffer, VulkanAllocator};
use std::sync::{Mutex, MutexGuard};

/// Unified interprocess memory handle wrapper for GPU and CPU shared memory.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum InterprocessMemoryHandle {
    /// Linux/Unix-like opaque file descriptor for Vulkan GPU memory.
    #[cfg(any(unix, target_os = "wasi"))]
    GpuOpaqueFd(u64),
    /// Linux/Unix-like file descriptor for CPU shared-memory transport.
    #[cfg(any(unix, target_os = "wasi"))]
    CpuSharedFd(u64),
    /// Windows opaque handle style identifier for Vulkan GPU memory.
    #[cfg(windows)]
    GpuOpaqueWin32Handle(u64),
    /// Windows mapping handle style identifier for CPU shared-memory transport.
    #[cfg(windows)]
    CpuSharedWin32Handle(u64),
}

impl InterprocessMemoryHandle {
    pub(crate) fn from_gpu_id(id: u64) -> Self {
        #[cfg(any(unix, target_os = "wasi"))]
        {
            Self::GpuOpaqueFd(id)
        }
        #[cfg(windows)]
        {
            Self::GpuOpaqueWin32Handle(id)
        }
    }

    pub(crate) fn from_cpu_shared_id(id: u64) -> Self {
        #[cfg(any(unix, target_os = "wasi"))]
        {
            Self::CpuSharedFd(id)
        }
        #[cfg(windows)]
        {
            Self::CpuSharedWin32Handle(id)
        }
    }

    /// Returns whether the wrapped handle value is non-zero.
    pub fn is_valid(&self) -> bool {
        match *self {
            #[cfg(any(unix, target_os = "wasi"))]
            Self::GpuOpaqueFd(raw) | Self::CpuSharedFd(raw) => raw != 0,
            #[cfg(windows)]
            Self::GpuOpaqueWin32Handle(raw) | Self::CpuSharedWin32Handle(raw) => raw != 0,
        }
    }
}

/// Preferred memory location and strategy hints for allocation.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MemoryLocation {
    /// Allocate GPU memory using Vulkan.
    GpuVulkan { device_id: u32 },
    /// Allocate host memory with optional GPU-accessibility.
    CpuHost {
        /// Whether host memory should be pinned for faster GPU staging copies.
        gpu_pinned: bool,
    },
}

/// Type-level metadata for allocated memory.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MemoryType {
    /// Vulkan GPU memory.
    GpuVulkan { device_id: u32 },
    /// Host CPU memory and selected strategy attributes.
    Cpu {
        strategy: CpuAllocationStrategy,
        gpu_pinned: bool,
        alignment: usize,
    },
}

#[derive(Debug)]
enum MemoryBufferBackend {
    Cpu(CpuMemoryBuffer),
    Gpu(GpuMemoryBuffer),
}

/// Unified memory buffer wrapper returned by the allocator.
#[derive(Debug)]
pub struct MemoryBuffer {
    backend: MemoryBufferBackend,
}

impl MemoryBuffer {
    /// Returns an immutable raw pointer to the beginning of the buffer.
    pub fn as_ptr(&self) -> *const u8 {
        match &self.backend {
            MemoryBufferBackend::Cpu(buffer) => buffer.as_ptr(),
            MemoryBufferBackend::Gpu(buffer) => buffer.as_ptr(),
        }
    }

    /// Returns a mutable raw pointer to the beginning of the buffer.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        match &mut self.backend {
            MemoryBufferBackend::Cpu(buffer) => buffer.as_mut_ptr(),
            MemoryBufferBackend::Gpu(buffer) => buffer.as_mut_ptr(),
        }
    }

    /// Returns the buffer size in bytes.
    pub fn size(&self) -> usize {
        match &self.backend {
            MemoryBufferBackend::Cpu(buffer) => buffer.size(),
            MemoryBufferBackend::Gpu(buffer) => buffer.size(),
        }
    }

    /// Returns structured memory type metadata.
    pub fn memory_type(&self) -> MemoryType {
        match &self.backend {
            MemoryBufferBackend::Cpu(buffer) => MemoryType::Cpu {
                strategy: buffer.strategy(),
                gpu_pinned: buffer.gpu_pinned(),
                alignment: buffer.alignment(),
            },
            MemoryBufferBackend::Gpu(buffer) => MemoryType::GpuVulkan {
                device_id: buffer.device_id(),
            },
        }
    }

    /// Returns `true` when the buffer is GPU-backed.
    pub fn is_gpu(&self) -> bool {
        matches!(self.backend, MemoryBufferBackend::Gpu(_))
    }

    /// Returns `true` when the buffer is CPU-backed.
    pub fn is_cpu(&self) -> bool {
        matches!(self.backend, MemoryBufferBackend::Cpu(_))
    }

    /// Exports an interprocess handle for GPU or CPU shared-memory transport.
    pub fn export_handle(&self) -> Result<InterprocessMemoryHandle> {
        match &self.backend {
            MemoryBufferBackend::Gpu(buffer) => Ok(buffer.external_handle()),
            MemoryBufferBackend::Cpu(buffer) => Ok(buffer.shared_handle()),
        }
    }
}

/// Unified allocator with GPU and CPU backends.
#[derive(Debug)]
pub struct MemoryAllocator {
    vulkan_allocator: Option<Mutex<VulkanAllocator>>,
    cpu_allocator: Mutex<CpuAllocator>,
}

impl Default for MemoryAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryAllocator {
    /// Creates a new unified memory allocator.
    pub fn new() -> Self {
        Self {
            vulkan_allocator: VulkanAllocator::probe().map(Mutex::new),
            cpu_allocator: Mutex::new(CpuAllocator::new()),
        }
    }

    /// Allocates memory at the requested location.
    pub fn allocate(&self, size: usize, location: MemoryLocation) -> Result<MemoryBuffer> {
        match location {
            MemoryLocation::GpuVulkan { device_id } => {
                let mut vulkan_allocator = self.lock_vulkan_allocator()?;
                let buffer = vulkan_allocator.allocate(size, device_id)?;
                Ok(MemoryBuffer {
                    backend: MemoryBufferBackend::Gpu(buffer),
                })
            }
            MemoryLocation::CpuHost { gpu_pinned } => {
                let mut cpu_allocator = self.lock_cpu_allocator()?;
                let cpu_buffer = if gpu_pinned {
                    cpu_allocator.allocate_gpu_pinned(size)?
                } else {
                    cpu_allocator.allocate_standard(size)?
                };
                Ok(MemoryBuffer {
                    backend: MemoryBufferBackend::Cpu(cpu_buffer),
                })
            }
        }
    }

    /// Attempts primary location first and falls back if provided.
    pub fn allocate_with_fallback(
        &self,
        size: usize,
        primary: MemoryLocation,
    ) -> Result<MemoryBuffer> {
        match self.allocate(size, primary) {
            Ok(buffer) => Ok(buffer),
            Err(primary_err) => {
                if let Some(fallback_location) = fallback_location(primary) {
                    self.allocate(size, fallback_location)
                } else {
                    Err(primary_err)
                }
            }
        }
    }

    fn lock_vulkan_allocator(&self) -> Result<MutexGuard<'_, VulkanAllocator>> {
        let vulkan_allocator = self
            .vulkan_allocator
            .as_ref()
            .ok_or(LavaFlowError::GpuBackendUnavailable)?;
        vulkan_allocator
            .lock()
            .map_err(|_| LavaFlowError::AllocatorStatePoisoned {
                component: "vulkan_allocator",
            })
    }

    fn lock_cpu_allocator(&self) -> Result<MutexGuard<'_, CpuAllocator>> {
        self.cpu_allocator
            .lock()
            .map_err(|_| LavaFlowError::AllocatorStatePoisoned {
                component: "cpu_allocator",
            })
    }
}

fn fallback_location(primary: MemoryLocation) -> Option<MemoryLocation> {
    match primary {
        MemoryLocation::GpuVulkan { .. } => Some(MemoryLocation::CpuHost { gpu_pinned: false }),
        MemoryLocation::CpuHost { gpu_pinned: true } => {
            Some(MemoryLocation::CpuHost { gpu_pinned: false })
        }
        MemoryLocation::CpuHost { gpu_pinned: false } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_allocation_uses_standard_strategy() {
        let allocator = MemoryAllocator::new();
        let buffer = allocator
            .allocate(2048, MemoryLocation::CpuHost { gpu_pinned: false })
            .expect("allocate");

        assert!(buffer.is_cpu());
        assert_eq!(
            buffer.memory_type(),
            MemoryType::Cpu {
                strategy: CpuAllocationStrategy::Standard,
                gpu_pinned: false,
                alignment: std::mem::align_of::<u8>(),
            }
        );
    }

    #[test]
    fn gpu_allocation_exports_external_handle() {
        let allocator = MemoryAllocator::new();
        let buffer = allocator
            .allocate(1024, MemoryLocation::GpuVulkan { device_id: 0 })
            .expect("allocate gpu");
        let handle = buffer.export_handle().expect("export");

        assert!(buffer.is_gpu());
        assert!(handle.is_valid());
        #[cfg(any(unix, target_os = "wasi"))]
        assert!(matches!(handle, InterprocessMemoryHandle::GpuOpaqueFd(_)));
        #[cfg(windows)]
        assert!(matches!(
            handle,
            InterprocessMemoryHandle::GpuOpaqueWin32Handle(_)
        ));
    }

    #[test]
    fn cpu_allocation_exports_shared_memory_handle() {
        let allocator = MemoryAllocator::new();
        let buffer = allocator
            .allocate(1024, MemoryLocation::CpuHost { gpu_pinned: false })
            .expect("allocate cpu");
        let handle = buffer
            .export_handle()
            .expect("expected shared-memory handle");
        assert!(handle.is_valid());
        #[cfg(any(unix, target_os = "wasi"))]
        assert!(matches!(handle, InterprocessMemoryHandle::CpuSharedFd(_)));
        #[cfg(windows)]
        assert!(matches!(
            handle,
            InterprocessMemoryHandle::CpuSharedWin32Handle(_)
        ));
    }

    #[test]
    fn fallback_uses_cpu_when_gpu_device_missing() {
        let allocator = MemoryAllocator::new();
        let buffer = allocator
            .allocate_with_fallback(512, MemoryLocation::GpuVulkan { device_id: 99 })
            .expect("fallback allocate");

        assert!(buffer.is_cpu());
    }

    #[test]
    fn fallback_keeps_gpu_pinned_cpu_when_primary_succeeds() {
        let allocator = MemoryAllocator::new();
        let buffer = allocator
            .allocate_with_fallback(512, MemoryLocation::CpuHost { gpu_pinned: true })
            .expect("fallback allocate");

        assert_eq!(
            buffer.memory_type(),
            MemoryType::Cpu {
                strategy: CpuAllocationStrategy::GpuPinned,
                gpu_pinned: true,
                alignment: std::mem::align_of::<u8>(),
            }
        );
    }
}
