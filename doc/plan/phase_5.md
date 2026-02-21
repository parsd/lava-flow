# Phase 5: Cross-API Synchronization

## TL;DR

Add cross-API synchronization primitives (external semaphores, timeline sync) and integrate them into the `Channel`
workflow so sync is implicit for users.

## Scope

- External semaphore export/import for Vulkan <-> CUDA/OpenCL/OpenGL
- Timeline semaphore support where available
- Channel-managed wait/signal around send/recv
- Sync helpers for common producer/consumer flows

## Deliverables

- Semaphore export/import APIs
- Channel-level sync integration (wait on recv, signal on send)
- Cross-API sync examples and tests
- Interop validation utilities

## Example (API Shape)

```rust
let tx = ChannelBuilder::sender(my_loc.clone(), peer_loc.clone()).build()?;
let rx = ChannelBuilder::receiver(my_loc, peer_loc)
    .with_allocator(gpu_allocator)
    .build()?;

tx.send(frame, &meta)?; // internally signals external semaphore
let (frame, meta) = rx.recv::<ImageMeta>()?; // internally waits on external semaphore
let used = meta.used_size();
```

## Related Docs

- [Interop Overview](../spec/interop/README.md)
- [CUDA Interop](../spec/interop/cuda_interop.md)
- [OpenCL Interop](../spec/interop/opencl_interop.md)
- [OpenGL Interop](../spec/interop/opengl_interop.md)
- [Channels Spec](../spec/channels.md)

## External References

- [Vulkan External Semaphore](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_semaphore.html)
- [CUDA External Semaphore](https://docs.nvidia.com/cuda/cuda-runtime-api/group__CUDART__EXTRES__INTEROP.html)
- [OpenCL External Semaphores](https://registry.khronos.org/OpenCL/specs/3.0-unified/html/OpenCL_Ext.html)
- [OpenGL Semaphore Extensions](https://registry.khronos.org/OpenGL/extensions/EXT/EXT_semaphore.txt)
