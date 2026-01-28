# CUDA Interoperability with lava-flow

## TL;DR

Import Vulkan-allocated memory from lava-flow into CUDA via external memory handles (fd on Linux, HANDLE on Windows).
This document keeps examples only; see NVIDIA docs for full requirements.

---

## When to Use

- You want CUDA kernels to read/write lava-flow buffers without copying.
- You need zero-copy exchange between Vulkan allocation and CUDA compute.

---

## Key Points (Short)

- Linux uses file descriptors; Windows uses HANDLEs.
- CUDA owns the external handle after import; do not close it manually.
- Imported memory cannot be freed with `cudaFree()`.

---

## Minimal Example

### Rust (lava-flow allocation)

```rust
use lava_flow::{MemoryAllocator, MemoryLocation};

let mut allocator = MemoryAllocator::new()?;
let buffer = allocator.allocate(
    1_000_000,
    MemoryLocation::GpuVulkan { device_id: 0 }
)?;

#[cfg(target_os = "linux")]
let handle = buffer.export_fd()?;

#[cfg(target_os = "windows")]
let handle = buffer.export_handle()?;
```

### CUDA (Linux import)

```cpp
cudaExternalMemoryHandleDesc desc = {};
desc.type = cudaExternalMemoryHandleTypeOpaqueFd;
desc.handle.fd = vulkan_fd;
desc.size = 1000000;

cudaExternalMemory_t ext_mem;
cudaImportExternalMemory(&ext_mem, &desc);

cudaExternalMemoryBufferDesc buffer_desc = {};
buffer_desc.offset = 0;
buffer_desc.size = 1000000;

void* cuda_ptr = nullptr;
cudaExternalMemoryGetMappedBuffer(&cuda_ptr, ext_mem, &buffer_desc);
```

---

## External References

- [CUDA external memory API](https://docs.nvidia.com/cuda/cuda-runtime-api/group__CUDART__EXTRES__INTEROP.html)
- [Vulkan external memory (Linux)](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory_fd.html)
- [Vulkan external memory (Windows)](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory_win32.html)
- [NVIDIA sample (Vulkan-CUDA interop)](https://github.com/NVIDIA/cuda-samples)

---

**Next:** [OpenCL Interoperability](opencl_interop.md) | [OpenGL Interoperability](opengl_interop.md)
