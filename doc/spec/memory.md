# Layer 1: Memory & Allocator Architecture

This document specifies the complete memory allocation layer for lava-flow, supporting both GPU and CPU memory.

## Overview

**Layer 1** provides unified memory management abstraction that handles:

- **GPU memory** (Vulkan-allocated with cross-API support)
- **CPU memory** (host-side buffers for staging and non-GPU workloads)
- **Unified API** for channel producers/consumers to allocate without caring about memory type

Channels ([Layer 2](channels.md)) operate over both GPU and CPU memory transparently.

## Design Principle

**Single allocation interface, multiple backing stores:**

```rust
pub trait MemoryAllocator {
    fn allocate(&mut self, size: usize, location: MemoryLocation) -> Result<MemoryBuffer, Error>;
}

pub enum MemoryLocation {
    GpuVulkan { device: u32 },          // GPU memory via Vulkan
    CpuHost { numa_node: Option<u32> }, // CPU host memory (aligned, NUMA-aware)
}
```

Benefits:

- Producer doesn't need to know memory type
- Scope detection can select appropriate memory automatically
- Channels work over GPU buffers or CPU buffers identically
- Future: heterogeneous compute (CPU -> GPU transfers)

---

## GPU Memory (Vulkan-Based)

### Allocation

```rust
pub struct GpuMemoryBuffer {
    pub device: VkDevice,
    pub memory: VkDeviceMemory,
    pub buffer: VkBuffer,
    pub size: usize,

    // External memory export capability
    pub external_handle: Option<ExternalMemoryHandle>,
}

pub enum ExternalMemoryHandle {
    #[cfg(target_os = "linux")]
    Fd(std::os::unix::io::RawFd),
    #[cfg(target_os = "windows")]
    Win32Handle(HANDLE),
    // DmaBufFd(i32),         // Future Linux: DMA-BUF file descriptor (preferred)
}
```

### Vulkan Allocator Properties

**Memory types:**

- DEVICE_LOCAL (fastest, on-GPU)
- HOST_VISIBLE (CPU accessible, slower)
- HOST_CACHED (host-visible cached)

**For external memory (Vulkan 1.2+):**

```cpp
VkMemoryDedicatedAllocateInfo dedicated = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO,
    .buffer = vk_buffer,
};

VkExternalMemoryBufferCreateInfo ext_info = {
    .sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO,
    .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT,  // Linux
};

VkExportMemoryAllocateInfo export_info = {
    .sType = VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO,
    .handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT,
};

// Allocate with exportable handle
VkDeviceMemory memory = vkAllocateMemory(device, &allocInfo, NULL);

// Export handle
VkMemoryGetFdInfoKHR fd_info = {
    .sType = VK_STRUCTURE_TYPE_MEMORY_GET_FD_INFO_KHR,
    .memory = memory,
    .handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT,
};
vkGetMemoryFdKHR(device, &fd_info, &fd);
```

### Cross-API Support

Vulkan external memory enables CUDA/OpenCL/OpenGL to import GPU buffers:

```rust
// lava-flow allocates
let gpu_buffer = allocator.allocate_gpu(1024*1024)?;
let handle = gpu_buffer.export_handle()?;

// CUDA imports (platform-specific handle use)
cudaExternalMemory_t cuda_mem;
cudaImportExternalMemory(&cuda_mem, &handle, ...);

// OpenCL imports
cl_mem opencl_buffer = clCreateBufferFromExternalMemory(...);
```

### Properties

- **Latency:** O(1) allocation (no copies)
- **Throughput:** Full GPU bandwidth (100+ GB/s)
- **Scope:** Vulkan device to any process on same machine
- **Exportable:** Yes, via `ExternalMemoryHandle`
- **Cross-API:** Yes (CUDA, OpenCL, OpenGL via standard extensions)

---

## CPU Memory (Host Buffers)

### Allocation

```rust
pub struct CpuMemoryBuffer {
    pub ptr: *mut u8,
    pub size: usize,
    pub alignment: usize,  // 4KB or 2MB for hugepages

    // NUMA awareness
    pub numa_node: Option<u32>,
    pub is_hugepage: bool,

    // Interop with GPU
    pub gpu_accessible: bool,  // Can GPU map this?
    pub pinned: bool,          // Is memory pinned (locked)?
}
```

### Allocation Strategies

#### 1. **Standard malloc (Default)**

```rust
pub fn allocate_cpu_standard(size: usize) -> Result<CpuMemoryBuffer> {
    let layout = Layout::from_size_align(size, 64)?;  // Cache-line aligned
    let ptr = alloc(layout);

    Ok(CpuMemoryBuffer {
        ptr,
        size,
        alignment: 64,
        numa_node: None,
        is_hugepage: false,
        gpu_accessible: false,
        pinned: false,
    })
}
```

**Properties:**

- Latency: O(1) allocation
- Alignment: Cache-line (64 bytes)
- Accessible from: CPU only
- Use case: Producer/consumer on same CPU, no GPU involvement

#### 2. **NUMA-Aware Allocation**

```rust
pub fn allocate_cpu_numa(size: usize, numa_node: u32) -> Result<CpuMemoryBuffer> {
    // Allocate on specific NUMA node
    let mut ptr = std::ptr::null_mut();
    numa_alloc_onnode(&mut ptr, size, numa_node as i32)?;

    Ok(CpuMemoryBuffer {
        ptr,
        size,
        alignment: 4096,
        numa_node: Some(numa_node),
        is_hugepage: false,
        gpu_accessible: false,
        pinned: false,
    })
}
```

**Properties:**

- Latency: O(1) allocation, reduced NUMA latency
- Alignment: Page-aligned (4KB)
- Accessible from: CPU on same NUMA node (preferred), other nodes (slower)
- Use case: Multi-socket systems where latency matters

#### 3. **GPU-Pinned Host Memory**

```rust
pub fn allocate_cpu_gpu_pinned(size: usize) -> Result<CpuMemoryBuffer> {
    // Allocate CPU memory and pin it for GPU access
    let layout = Layout::from_size_align(size, 4096)?;
    let ptr = alloc(layout);

    // Pin for GPU (example: CUDA)
    cudaHostRegister(ptr, size, cudaHostRegisterDefault)?;

    Ok(CpuMemoryBuffer {
        ptr,
        size,
        alignment: 4096,
        numa_node: None,
        is_hugepage: false,
        gpu_accessible: true,
        pinned: true,
    })
}
```

**Note (Vulkan):** There is no direct Vulkan equivalent to `cudaHostRegisterDefault` that pins an arbitrary host
pointer. The closest options are allocating `HOST_VISIBLE` Vulkan memory and mapping it, or using
`VK_EXT_external_memory_host` where supported to import host pointers, but support is limited and not portable.

**Properties:**

- Latency: O(1) allocation, PCIe latency for GPU access
- Alignment: Page-aligned (4KB)
- Accessible from: CPU (cache-coherent), GPU (slower than DEVICE_LOCAL)
- Use case: CPU ↔ GPU staging buffer

#### 4. **Huge Pages**

```rust
pub fn allocate_cpu_hugepage(size: usize, numa_node: Option<u32>) -> Result<CpuMemoryBuffer> {
    // Allocate 2MB pages (Linux /dev/hugepages)
    let mut ptr = std::ptr::null_mut();
    let fd = open("/dev/hugepages/tmp_lava_flow", O_RDWR | O_CREAT)?;
    ptr = mmap(std::ptr::null_mut(), size, PROT_READ | PROT_WRITE,
               MAP_SHARED | MAP_HUGETLB, fd, 0)?;

    Ok(CpuMemoryBuffer {
        ptr,
        size,
        alignment: 2 * 1024 * 1024,  // 2MB pages
        numa_node,
        is_hugepage: true,
        gpu_accessible: false,
        pinned: false,
    })
}
```

**Properties:**

- Latency: O(1) allocation, reduced TLB misses
- Alignment: 2MB (or 1GB on some systems)
- Accessible from: CPU with better cache efficiency
- Use case: Large frame buffers, reducing memory pressure

### CPU Memory Properties

| Strategy | Latency | Throughput | GPU Access | Use Case |
| -------- | ------- | ---------- | ---------- | -------- |
| **Standard malloc** | O(1) | CPU bandwidth | No | CPU-only channels |
| **NUMA-aware** | O(1) + NUMA latency | CPU bandwidth | No | Multi-socket systems |
| **GPU-pinned** | O(1) + PCIe latency | PCIe bandwidth | Yes | CPU<->GPU staging |
| **Huge pages** | O(1) + reduced TLB | CPU bandwidth | No | Large buffers |

---

## Unified Allocator API

### Public Interface

```rust
pub struct MemoryAllocator {
    vulkan_context: VulkanContext,  // GPU allocator
    cpu_allocator: CpuAllocator,    // CPU allocator
}

pub enum MemoryLocation {
    GpuVulkan { device_id: u32 },
    CpuHost {
        numa_node: Option<u32>,
        gpu_accessible: bool,
        use_hugepages: bool,
    },
}

pub trait MemoryBuffer {
    fn as_ptr(&self) -> *const u8;
    fn as_mut_ptr(&mut self) -> *mut u8;
    fn size(&self) -> usize;
    fn memory_type(&self) -> MemoryType;
    fn is_gpu(&self) -> bool;
    fn is_cpu(&self) -> bool;
}

pub enum MemoryType {
    Gpu { format: ExportFormat },
    Cpu { pinned: bool, numa_node: Option<u32> },
}

impl MemoryAllocator {
    /// Allocate memory at specified location
    pub fn allocate(
        &mut self,
        size: usize,
        location: MemoryLocation,
    ) -> Result<Box<dyn MemoryBuffer>> {
        match location {
            MemoryLocation::GpuVulkan { device_id } => {
                self.vulkan_context.allocate(size, device_id)
            }
            MemoryLocation::CpuHost { numa_node, gpu_accessible, use_hugepages } => {
                if gpu_accessible {
                    self.cpu_allocator.allocate_gpu_pinned(size)
                } else if use_hugepages {
                    self.cpu_allocator.allocate_hugepage(size, numa_node)
                } else if let Some(node) = numa_node {
                    self.cpu_allocator.allocate_numa(size, node)
                } else {
                    self.cpu_allocator.allocate_standard(size)
                }
            }
        }
    }

    /// Allocate with automatic location selection (Phase 2 / scope detection)
    pub fn allocate_auto(
        &mut self,
        size: usize,
        my_location: &ProcessLocation,
        peer_location: &ProcessLocation,
        channel_profile: ChannelProfile,
    ) -> Result<Box<dyn MemoryBuffer>> {
        let scope = CommunicationScope::from_locations(my_location, peer_location);

        let location = select_memory_location(scope, channel_profile);
        self.allocate(size, location)
    }
}
```

### Usage Patterns

#### Pattern 1: GPU Channel (Local)

```rust
// Allocate GPU buffer with automatic export
let buffer = allocator.allocate_auto(
    1MB,
    &my_location,
    &peer_location,
    ChannelProfile::GpuPreferred,
)?;
// Returns: GPU buffer with export handle

// Send via channel (zero-copy via shared GPU memory)
channel.send(frame_with_buffer)?;
```

#### Pattern 2: CPU Channel (Multi-Node)

```rust
// Allocate CPU buffer with MPI transport
let buffer = allocator.allocate_auto(
    1MB,
    &my_location,
    &peer_location,
    ChannelProfile::CpuShared,
)?;
// Returns: CPU buffer (huge pages for efficiency)

// Send via channel (MPI serialization)
channel.send(frame_with_buffer)?;
```

#### Pattern 3: Explicit GPU Allocation

```rust
let gpu_buffer = allocator.allocate(
    1MB,
    MemoryLocation::GpuVulkan { device_id: 0 }
)?;
```

#### Pattern 4: Explicit CPU Allocation (Staging)

```rust
let cpu_buffer = allocator.allocate(
    1MB,
    MemoryLocation::CpuHost {
        numa_node: Some(0),
        gpu_accessible: true,   // Pinned for GPU
        use_hugepages: false,   // Staging buffers small
    }
)?;
```

---

## Memory Type Detection

### Scope Detection Extended

Current scope detection (Phase 1) only determines transport. Layer 1 adds memory type selection:

```rust
pub fn select_memory_location(
    scope: CommunicationScope,
    channel_profile: ChannelProfile,
) -> MemoryLocation {
    match (scope, channel_profile) {
        (CommunicationScope::Local, ChannelProfile::GpuPreferred) => {
            // GPU memory: full bandwidth
            MemoryLocation::GpuVulkan { device_id: 0 }
        }
        (CommunicationScope::Local, ChannelProfile::CpuShared) => {
            // CPU memory: shared memory on same host
            MemoryLocation::CpuHost {
                numa_node: None,
                gpu_accessible: false,
                use_hugepages: false,
            }
        }
        (CommunicationScope::Remote, ChannelProfile::HighThroughput) => {
            // CPU with hugepages: MPI over large buffers
            MemoryLocation::CpuHost {
                numa_node: None,
                gpu_accessible: false,
                use_hugepages: true,
            }
        }
        (CommunicationScope::Remote, ChannelProfile::LowLatency) => {
            // CPU small buffer: MPI latency-optimized
            MemoryLocation::CpuHost {
                numa_node: None,
                gpu_accessible: false,
                use_hugepages: false,
            }
        }
    }
}

pub enum ChannelProfile {
    HighThroughput,  // Large frames, network bandwidth-bound
    LowLatency,      // Small messages, latency-bound
    GpuPreferred,    // GPU buffers (local only)
    CpuShared,       // CPU shared memory (local only)
}
```

---

## Memory Lifecycle

### Allocation -> Export -> Import -> Free

#### GPU Memory

```rust
// Phase 1: Allocate
let gpu_buffer = vulkan_allocator.allocate(size)?;

// Phase 2: Export (platform-agnostic)
let handle = gpu_buffer.export_handle()?;

// Phase 3: Send to peer (via IPC or MPI)
ipc_bus.send_handle(handle, destination)?;

// Phase 4: Peer imports
let imported = peer_vulkan_allocator.import_handle(handle)?;

// Phase 5: Use (shared access)
gpu_kernel.write_to(imported)?;

// Phase 6: Free
drop(imported);  // Decref
drop(gpu_buffer); // Decref
```

#### CPU Memory

```rust
// Allocate
let cpu_buffer = cpu_allocator.allocate_standard(size)?;

// Use directly (pointer-based access)
unsafe {
    std::ptr::write(cpu_buffer.as_mut_ptr(), data);
}

// Free
drop(cpu_buffer);
```

---

## Error Handling

### Allocation Errors

```rust
#[derive(Error, Debug)]
pub enum AllocationError {
    #[error("Out of GPU memory: requested {requested} bytes, available {available}")]
    GpuOutOfMemory { requested: usize, available: usize },

    #[error("Out of CPU memory: requested {requested} bytes")]
    CpuOutOfMemory { requested: usize },

    #[error("NUMA node {node} not available (max: {max})")]
    NumaNodeNotFound { node: u32, max: u32 },

    #[error("Hugepages not available (requested {size} bytes)")]
    HugepagesUnavailable { size: usize },

    #[error("GPU external memory not supported: {reason}")]
    ExternalMemoryNotSupported { reason: String },

    #[error("GPU device {device} not found")]
    GpuDeviceNotFound { device: u32 },

    #[error("GPU pinning failed: {reason}")]
    GpuPinningFailed { reason: String },
}
```

### Fallback Strategies

```rust
pub fn allocate_with_fallback(
    allocator: &mut MemoryAllocator,
    size: usize,
    primary: MemoryLocation,
    fallback: Option<MemoryLocation>,
) -> Result<Box<dyn MemoryBuffer>> {
    // Try primary location
    match allocator.allocate(size, primary.clone()) {
        Ok(buffer) => Ok(buffer),
        Err(e) => {
            tracing::warn!("Primary allocation failed: {:?}, trying fallback", e);
            if let Some(fb) = fallback {
                allocator.allocate(size, fb)
            } else {
                Err(e)
            }
        }
    }
}
```

**Example:** Try GPU memory, fall back to CPU if unavailable

```rust
allocate_with_fallback(
    allocator,
    1MB,
    MemoryLocation::GpuVulkan { device_id: 0 },
    Some(MemoryLocation::CpuHost {
        numa_node: None,
        gpu_accessible: false,
        use_hugepages: true,
    }),
)?
```

---

## Performance Characteristics

### Allocation Latency

| Strategy | Time | Notes |
| -------- | ---- | ----- |
| GPU (Vulkan) | ~100 us | Device allocation |
| CPU (standard) | ~1 us | malloc from heap |
| CPU (NUMA) | ~1 us | libnuma allocation |
| CPU (hugepage) | ~10 us | mmap system call |
| CPU (GPU-pinned) | ~10 us | cudaHostRegister |

### Memory Throughput

| Location | Throughput | Notes |
| -------- | ---------- | ----- |
| GPU DEVICE_LOCAL | 100-900 GB/s | GPU↔GPU bandwidth |
| GPU HOST_VISIBLE | 10-50 GB/s | GPU↔CPU via PCIe |
| CPU (intra-socket) | 50-100 GB/s | Memory bandwidth |
| CPU (cross-socket) | 10-50 GB/s | NUMA latency |
| Network (MPI) | 1-100 GB/s | Network bandwidth |

### Cache Efficiency

| Strategy | Benefit | Use Case |
| -------- | ------- | -------- |
| Cache-line aligned (64B) | CPU cache efficiency | Small frequent access |
| NUMA-aware | Reduced NUMA latency | Multi-socket systems |
| Hugepages (2MB) | Reduced TLB misses | Large buffers |
| GPU-pinned | PCIe optimization | GPU staging |

---

## Platform-Specific Considerations

### Linux

- **Hugepages:** supported via `/dev/hugepages`
- **NUMA:** libnuma for topology and allocation
- **GPU pinning:** CUDA APIs
- **External memory:** VkExternalMemory via `ExternalMemoryHandle`

### Windows

- **Hugepages:** Not directly supported (use aligned malloc)
- **NUMA:** Windows API (GetNumaNodeProcessorMask)
- **GPU pinning:** CUDA APIs
- **External memory:** VkExternalMemory via `ExternalMemoryHandle`

---

## Testing Requirements

### Unit Tests

```rust
#[test]
fn test_gpu_allocation() {
    let mut allocator = MemoryAllocator::new();
    let buffer = allocator.allocate(
        1024*1024,
        MemoryLocation::GpuVulkan { device_id: 0 }
    ).expect("GPU allocation failed");

    assert_eq!(buffer.size(), 1024*1024);
    assert!(buffer.is_gpu());
}

#[test]
fn test_cpu_numa_allocation() {
    let mut allocator = MemoryAllocator::new();
    let buffer = allocator.allocate(
        1024*1024,
        MemoryLocation::CpuHost {
            numa_node: Some(0),
            gpu_accessible: false,
            use_hugepages: false,
        }
    ).expect("NUMA allocation failed");

    assert_eq!(buffer.size(), 1024*1024);
    assert!(buffer.is_cpu());
}

#[test]
fn test_gpu_external_memory_export() {
    let mut allocator = MemoryAllocator::new();
    let gpu_buffer = allocator.allocate(
        1024*1024,
        MemoryLocation::GpuVulkan { device_id: 0 }
    ).expect("GPU allocation failed");

    let handle = gpu_buffer.export_handle().expect("Export failed");
    assert!(handle.is_valid());
}

#[test]
fn test_allocation_fallback() {
    let mut allocator = MemoryAllocator::new();

    // Try GPU, fallback to CPU
    let buffer = allocate_with_fallback(
        &mut allocator,
        1024*1024,
        MemoryLocation::GpuVulkan { device_id: 0 },
        Some(MemoryLocation::CpuHost {
            numa_node: None,
            gpu_accessible: false,
            use_hugepages: true,
        })
    ).expect("Allocation failed");

    // Should have allocated something
    assert_eq!(buffer.size(), 1024*1024);
}
```

### Integration Tests

- Cross-process GPU memory sharing (export/import)
- CPU<->GPU staging buffer performance
- NUMA placement verification (numactl check)
- Channel usage with both GPU and CPU buffers

---

## Conclusion

**Layer 1** now provides:

1. **GPU Memory (Vulkan)**
   - Cross-API support (CUDA, OpenCL, OpenGL)
   - External memory export (`ExternalMemoryHandle`)
   - Zero-copy inter-process sharing

2. **CPU Memory (Host Buffers)**
   - Standard malloc (default)
   - NUMA-aware allocation (multi-socket)
   - GPU-pinned buffers (staging)
   - Hugepages (large buffers)

3. **Unified API**
   - Single `MemoryAllocator` interface
   - Automatic location selection based on scope
   - Fallback strategies
   - Error handling
