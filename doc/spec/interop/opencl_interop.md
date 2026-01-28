# OpenCL Interoperability with lava-flow

## TL;DR

Import Vulkan-allocated memory into OpenCL via `clCreateBufferWithProperties()` using external memory handles (fd on
Linux, HANDLE on Windows). This document is example-only; refer to Khronos for full requirements.

---

## When to Use

- You want OpenCL kernels to read/write lava-flow buffers without copying.
- You need zero-copy exchange between Vulkan allocation and OpenCL compute.

---

## Key Points (Short)

- Requires `cl_khr_external_memory` support.
- Linux uses file descriptors; Windows uses HANDLEs.
- Do not close external handles manually after import.

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

### OpenCL (Linux import)

```c
cl_mem_properties props[] = {
    CL_EXTERNAL_MEMORY_HANDLE_OPAQUE_FD_KHR, (cl_mem_properties)vulkan_fd,
    0
};

cl_mem buffer = clCreateBufferWithProperties(
    context,
    props,
    CL_MEM_READ_WRITE,
    1000000,
    NULL,
    &err
);
```

---

## External References

- [OpenCL external memory](https://registry.khronos.org/OpenCL/specs/3.0-unified/html/OpenCL_Ext.html#cl_khr_external_memory)
- [Vulkan external memory (Linux)](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory_fd.html)
- [Vulkan external memory (Windows)](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory_win32.html)
- [Khronos interop guide](https://www.khronos.org/opengl/wiki/Vulkan_and_OpenCL_Interoperability)

---

**Next:** [OpenGL Interoperability](opengl_interop.md) | [CUDA Interoperability](cuda_interop.md)
