# ADR-003: External Memory Handle Types

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

Use `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT` (file descriptors) on Linux and
`VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT` (`HANDLE` objects) on Windows for Vulkan external memory sharing.
DMA-BUF support deferred to Phase 5+ as optional optimization.

## Problem

Vulkan supports multiple external memory handle types for cross-process GPU memory sharing:

- **Opaque FD** (Linux): Standard file descriptor
- **DMA-BUF** (Linux): GPU-aware buffer sharing (newer)
- **Opaque Win32 HANDLE** (Windows): NT kernel handle
- **D3D11/D3D12 resources** (Windows): Direct3D interop

Must choose primary handle type per platform that balances:

- **Compatibility:** Works on all drivers
- **Performance:** Minimal overhead
- **Portability:** Cross-API import support

## Decision

- **Linux:** `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT` (file descriptors)
- **Windows:** `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT` (NT HANDLE)
- **Future (Phase 5+):** Add `VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT` as optimization

## Rationale

### Linux: Opaque FD

**Pros:**

- Universal driver support (NVIDIA 460+, AMD AMDGPU 5.x+, Intel Xe+)
- Cross-API import (CUDA `cudaExternalMemoryHandleTypeOpaqueFd`)
- Simple semantics (standard file descriptor)
- Kernel-level security (file descriptor permissions)

**Cons:**

- Not GPU-aware (kernel treats as opaque)
- Slightly slower than DMA-BUF on newer systems

**DMA-BUF Alternative:**

- GPU-aware (kernel understands it's GPU memory)
- Potentially faster (optimized path in kernel)
- Newer (requires Linux 5.10+, driver support varies)
- Less compatible with older drivers

**Decision:** Opaque FD for Phase 2-4 (maximum compatibility), DMA-BUF as optional in Phase 5+.

### Windows: Opaque Win32 HANDLE

**Pros:**

- Standard NT kernel handle
- Driver support (NVIDIA 460+, AMD 22.50+)
- Cross-API import (CUDA `cudaExternalMemoryHandleTypeOpaqueWin32`)

**Cons:**

- Windows-specific (not portable)

**D3D12 Resource Alternative:**

- Native Windows (Direct3D interop)
- Ties to D3D12 API (less portable)
- No CUDA import support

**Decision:** Opaque Win32 HANDLE (simpler, cross-API).

## Implementation

```rust
#[cfg(target_os = "linux")]
pub const EXTERNAL_MEMORY_HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;

#[cfg(target_os = "windows")]
pub const EXTERNAL_MEMORY_HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32;

pub struct ExternalMemoryHandle {
    #[cfg(target_os = "linux")]
    pub fd: RawFd,

    #[cfg(target_os = "windows")]
    pub handle: HANDLE,
}
```

## Consequences

- **Maximum compatibility** — Works on all major drivers
- **Cross-API import** — CUDA/OpenCL can import
- **Not GPU-aware** — DMA-BUF potentially faster (future optimization)
