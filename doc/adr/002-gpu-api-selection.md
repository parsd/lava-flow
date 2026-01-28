﻿# ADR-002: GPU API Selection

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

Use Vulkan for all GPU memory allocation instead of CUDA, HIP, OpenGL, or Direct3D 12 because it's vendor-neutral
(ISO/IEC 23028), has first-class external memory support, and allows cross-API import (CUDA/OpenCL/OpenGL can import
Vulkan memory).

## Problem

lava-flow needs to allocate GPU memory that can be:

1. **Shared between processes** (via OS handles: `fd` on Linux, `HANDLE` on Windows)
2. **Imported by other GPU APIs** (CUDA kernels, OpenCL, OpenGL shaders)
3. **Vendor-neutral** (works on NVIDIA, AMD, Intel GPUs)
4. **High-performance** (100-900 GB/s throughput)

No single GPU API satisfies all requirements:

- **CUDA:** NVIDIA-only, no AMD/Intel support
- **HIP:** AMD-focused, limited NVIDIA support
- **OpenGL:** Deprecated, no modern external memory
- **Direct3D 12:** Windows-only, no Linux/macOS
- **Metal:** macOS-only, incompatible with discrete GPU model

## Decision

**Use Vulkan 1.2+ as primary GPU memory allocator.**

**Rationale:**

- **Vendor-neutral:** NVIDIA, AMD, Intel all support Vulkan
- **Standardized:** ISO/IEC 23028 (not proprietary like CUDA)
- **First-class external memory:** `VK_KHR_external_memory` core in 1.2+
- **Cross-API import:** CUDA/OpenCL/OpenGL have extensions to import Vulkan memory
- **Modern:** Active development, future-proof
- **Cross-platform:** Linux, Windows, Android (macOS via MoltenVK, limited)

## Alternatives Considered

### Alternative 1: CUDA

**CUDA Memory Allocation:**

```cpp
cudaMalloc(&ptr, size);
cudaExportMemory(&handle, ptr);  // cudaExternalMemoryGetMappedBuffer
```

**Pros:**

- Most mature GPU API (since 2007)
- Best documentation
- Largest ecosystem (PyTorch, TensorFlow use CUDA)
- GPU-direct RDMA support

**Cons:**

- **NVIDIA-only** — Locks library to single vendor
- **Proprietary** — No open standard
- **External memory second-class** — Added late (CUDA 11+)
- **No AMD/Intel** — Can't run on Radeon/Arc GPUs
- **License restrictions** — CUDA EULA limits reverse engineering

**Rejected:** Vendor lock-in unacceptable for infrastructure library.

---

### Alternative 2: HIP (Heterogeneous Interface for Portability)

**HIP Memory Allocation:**

```cpp
hipMalloc(&ptr, size);
hipExportMemory(&handle, ptr);
```

**Pros:**

- AMD's answer to CUDA portability
- Can compile to CUDA (via hipify) or ROCm (AMD)
- Syntax similar to CUDA

**Cons:**

- **AMD-focused** — Optimized for Radeon, NVIDIA support incomplete
- **NVIDIA support via translation** — hipify converts to CUDA (extra layer)
- **Smaller ecosystem** — Less mature than CUDA or Vulkan
- **External memory unclear** — Documentation sparse

**Rejected:** Doesn't solve vendor-neutrality (still AMD-centric).

---

### Alternative 3: OpenGL

**OpenGL Memory Allocation:**

```c
glGenBuffers(1, &buffer);
glBindBuffer(GL_SHADER_STORAGE_BUFFER, buffer);
glBufferStorage(GL_SHADER_STORAGE_BUFFER, size, NULL, flags);
```

**Pros:**

- Cross-platform (Windows, Linux, macOS)
- Vendor-neutral
- Older ecosystem (1992+)

**Cons:**

- **Deprecated** — Khronos Group favors Vulkan
- **No modern external memory** — `GL_EXT_memory_object` exists but not widely supported
- **Graphics-focused** — Not designed for compute
- **Inefficient** — Higher overhead than Vulkan/CUDA
- **State machine** — Global state makes IPC harder

**Rejected:** Legacy API, Vulkan supersedes it.

---

### Alternative 4: Direct3D 12

**D3D12 Memory Allocation:**

```cpp
device->CreateCommittedResource(...);
device->CreateSharedHandle(resource, &handle);
```

**Pros:**

- Windows-native
- Modern API (2015+)
- External memory support via `ID3D12Heap`

**Cons:**

- **Windows-only** — No Linux/macOS support
- **Microsoft-proprietary** — Not open standard
- **No cross-platform** — Can't run on HPC clusters (mostly Linux)

**Rejected:** Platform lock-in unacceptable.

---

### Alternative 5: Metal

**Metal Memory Allocation:**

```swift
let buffer = device.makeBuffer(length: size, options: .storageModeShared)
```

**Pros:**

- Native to macOS
- Unified memory model (CPU/GPU shared)

**Cons:**

- **macOS-only** — No Linux/Windows support
- **Unified memory** — Can be handled via CPU shared memory
- **Apple-proprietary** — Not open standard

**Rejected:** Platform lock-in, architecture mismatch (unified vs discrete).

---

### Alternative 6: Vulkan **CHOSEN**

**Vulkan Memory Allocation:**

```rust
let memory = device.allocate_memory(&AllocateInfo {
    allocation_size: size,
    memory_type_index: device_local_index,
    ..Default::default()
}, None)?;

// Export for IPC
let handle = device.get_memory_fd(&MemoryGetFdInfoKHR {
    memory,
    handle_type: ExternalMemoryHandleType::OPAQUE_FD,
    ..Default::default()
})?;
```

**Pros:**

- **Vendor-neutral** — NVIDIA, AMD, Intel all support
- **ISO/IEC 23028 standard** — Open, vendor-agnostic
- **First-class external memory** — Core feature in 1.2+
- **Cross-API import** — CUDA/OpenCL/OpenGL can import Vulkan handles
- **Modern** — Designed for GPU compute (not just graphics)
- **Efficient** — Explicit control, no hidden state
- **Cross-platform** — Linux, Windows, Android

**Cons:**

- **Verbose API** — More boilerplate than CUDA (mitigated by Rust crates)
- **macOS limited** — MoltenVK (Vulkan-on-Metal) incomplete, but macOS uses unified GPU/CPU memory where shared CPU memory
  can be used.

**Chosen:** Best balance of vendor-neutrality, standards, and functionality.

## Implementation

### Vulkan Memory Types

```rust
// Phase 2: GPU memory allocation
pub enum GpuMemoryType {
    DeviceLocal,   // VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT
                   // Fastest: 100-900 GB/s, GPU-only

    HostVisible,   // VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT
                   // CPU-accessible: 10-50 GB/s, PCIe bandwidth

    HostCached,    // VK_MEMORY_PROPERTY_HOST_CACHED_BIT
                   // CPU-cached: 50-100 GB/s, good for staging
}
```

### External Memory Export

```rust
// Linux (file descriptor)
let fd = device.get_memory_fd(&vk::MemoryGetFdInfoKHR {
    memory,
    handle_type: vk::ExternalMemoryHandleTypeFlagsKHR::OPAQUE_FD,
    ..Default::default()
})?;

// Windows (HANDLE)
let handle = device.get_memory_win32_handle(&vk::MemoryGetWin32HandleInfoKHR {
    memory,
    handle_type: vk::ExternalMemoryHandleTypeFlagsKHR::OPAQUE_WIN32,
    ..Default::default()
})?;
```

### Cross-API Import (Phase 5+)

**CUDA imports Vulkan memory:**

```cpp
// CUDA extension: cudaExternalMemory
cudaExternalMemoryHandleDesc desc = {};
desc.type = cudaExternalMemoryHandleTypeOpaqueFd;
desc.handle.fd = vulkan_fd;
desc.size = size;

cudaExternalMemory_t ext_mem;
cudaImportExternalMemory(&ext_mem, &desc);

void* cuda_ptr;
cudaExternalMemoryGetMappedBuffer(&cuda_ptr, ext_mem, &buffer_desc);
// Now CUDA kernel can access Vulkan memory
```

**OpenCL imports Vulkan memory:**

```c
// OpenCL extension: cl_khr_external_memory
cl_mem_properties props[] = {
    CL_EXTERNAL_MEMORY_HANDLE_OPAQUE_FD_KHR, vulkan_fd,
    0
};
cl_mem buffer = clCreateBufferWithProperties(context, props, CL_MEM_READ_WRITE, size, NULL, &err);
```

**OpenGL imports Vulkan memory:**

```c
// OpenGL extension: GL_EXT_memory_object
GLuint mem_obj;
glCreateMemoryObjectsEXT(1, &mem_obj);
glImportMemoryFdEXT(mem_obj, size, GL_HANDLE_TYPE_OPAQUE_FD_EXT, vulkan_fd);

GLuint buffer;
glCreateBuffers(1, &buffer);
glNamedBufferStorageMemEXT(buffer, size, mem_obj, 0);
```

## Consequences

### Positive

- **Vendor-neutral** — Works on NVIDIA, AMD, Intel GPUs without code change
- **Cross-API** — CUDA, OpenCL, OpenGL can import Vulkan memory
- **Future-proof** — Active standard (Vulkan 1.3 released 2022)
- **High-performance** — Same throughput as CUDA (100-900 GB/s)
- **Open standard** — ISO/IEC 23028, not proprietary

### Negative

- **Verbose API** — More code than CUDA (mitigated by `ash` and `gpu-allocator` crates)
- **Learning curve** — Developers familiar with CUDA need to learn Vulkan - can be mitigated by library code
- **macOS limited** — MoltenVK incomplete, unified memory model - use CPU shared memory

### Trade-offs Accepted

- **Verbosity:** Worth it for vendor-neutrality
- **Learning curve:** One-time cost, documentation/implementation helps
- **macOS:** Acceptable limitation

## Testing

```rust
#[test]
fn test_vulkan_device_available() {
    let allocator = MemoryAllocator::new();
    assert!(allocator.has_vulkan_device());
}

#[test]
fn test_gpu_memory_allocation() {
    let allocator = MemoryAllocator::new();
    let buffer = allocator.allocate(
        1_000_000,
        MemoryLocation::GpuVulkan { device_id: 0 }
    ).expect("allocate GPU memory");
    assert_eq!(buffer.size(), 1_000_000);
}

#[test]
fn test_external_memory_export() {
    let buffer = allocate_gpu_buffer(1_000_000)?;
    let handle = buffer.export_handle()?;
    #[cfg(target_os = "linux")]
    assert!(handle.fd > 0);
    #[cfg(target_os = "windows")]
    assert!(!handle.handle.is_null());
}
```

## References

- **Vulkan Specification:** <https://registry.khronos.org/vulkan/#apispecs>
- **VK_KHR_external_memory:** <https://registry.khronos.org/vulkan/specs/1.3-extensions/man/html/VK_KHR_external_memory.html>
- **CUDA External Memory:** <https://docs.nvidia.com/cuda/cuda-driver-api/group__CUDA__EXTRES__INTEROP.html>
- **[GPU memory allocation details](../spec/architecture.md)**

---

**Decision:** Vulkan for vendor-neutral, standardized, cross-API GPU memory allocation.

**Rationale:** Only API that meets all requirements (vendor-neutral, external memory, cross-API import, modern
standard).
