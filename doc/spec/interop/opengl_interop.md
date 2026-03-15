# OpenGL Interoperability with lava-flow

## TL;DR

Import Vulkan-allocated memory into OpenGL via `GL_EXT_memory_object` using external memory handles (fd on Linux,
HANDLE on Windows). This document is example-only; see Khronos for full details.

---

## When to Use

- You want OpenGL to read/write lava-flow buffers or textures without copying.
- You need zero-copy exchange between Vulkan allocation and OpenGL rendering.

---

## Key Points (Short)

- Requires `GL_EXT_memory_object` (plus fd/win32 variants).
- Linux uses file descriptors; Windows uses HANDLEs.
- Do not close external handles manually after import.

---

## Minimal Example

### Rust (lava-flow allocation, illustrative)

```rust
use lava_flow::gpu;

let mut allocator = gpu::Allocator::new_for_device(0)?;
let buffer = allocator.allocate(1_000_000)?;
// External handle export is performed by interop/channel layer APIs.
```

### OpenGL (Linux import)

```c
GLuint mem_obj;
glCreateMemoryObjectsEXT(1, &mem_obj);

glImportMemoryFdEXT(
    mem_obj,
    1000000,
    GL_HANDLE_TYPE_OPAQUE_FD_EXT,
    vulkan_fd
);

GLuint buffer;
glCreateBuffers(1, &buffer);
glNamedBufferStorageMemEXT(buffer, 1000000, mem_obj, 0);
```

---

## External References

- [OpenGL memory object](https://registry.khronos.org/OpenGL/extensions/EXT/EXT_external_objects.txt)
- [OpenGL fd interop](https://khronos.org/registry/OpenGL/extensions/EXT/EXT_external_objects_fd.txt)
- [OpenGL win32 interop](https://khronos.org/registry/OpenGL/extensions/EXT/EXT_external_objects_win32.txt)
- [Vulkan external memory (Linux)](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory_fd.html)
- [Vulkan external memory (Windows)](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory_win32.html)

---

**See also:** [CUDA Interoperability](cuda_interop.md) | [OpenCL Interoperability](opencl_interop.md)
