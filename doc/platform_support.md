# Platform Support: Detailed Feature Matrix

**TL;DR:** Linux has full support (all features, all optimizations). Windows has stable basic support (Vulkan + TCP
MPI). macOS is using shared CPU memory.

## Linux (Full Support)

### Feature Matrix

| Feature | Status | Notes |
| ------- | ------ | ----- |
| **Vulkan GPU** | Full | NVIDIA 460+, AMD RDNA+, Intel Arc+ |
| **External Memory FD** | Full | `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT` |
| **Vulkan IPC** | Optimized | < 100 ns, kernel-bypass |
| **CPU malloc** | Yes (shared memory) | Standard libc |
| **CPU NUMA** | Yes | libnuma for topology |
| **CPU hugepages** | Yes | `/proc/sys/vm/hugetlb_*` |
| **CPU GPU-pinned** | Yes | `cudaHostRegister()` or equivalent |
| **MPI** | Full | OpenMPI 4.0+, MPICH 3.3+ |
| **MPI RDMA** | Yes | UCX transports, InfiniBand/RoCE |
| **MPI GPU-aware** | Future | Phase 4+ |

### Tested Configurations

**NVIDIA:**

- Driver: 470.x+
- GPU: RTX 3070
- Vulkan: 1.3+
- MPI: OpenMPI 4.1+

**Intel:**

- Driver: Xe DG2+ (Arc A-series)
- GPU: UHD
- Vulkan: 1.3+
- MPI: OpenMPI 4.1+

### Known Issues

| Issue | Impact | Workaround |
| ----- | ------ | ---------- |
| NVIDIA driver 470.x external memory bug | Occasional crashes | Update to 475+ |
| OpenMPI 4.0 RDMA selection | Suboptimal routing | Update to 4.1+ |

---

## Windows (Stable, Partial Support)

### Feature Matrix

| Feature | Status | Notes |
| ------- | ------ | ----- |
| **Vulkan GPU** | Full | NVIDIA 470+, AMD (via driver) |
| **External Memory HANDLE** | Full | `VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT` |
| **Vulkan IPC** | Works | < 100 ns on same machine |
| **CPU malloc** | Yes (shared memory) | Windows CRT |
| **CPU NUMA** | Partial | Windows API (less optimized than Linux) |
| **CPU hugepages** | No | Large Pages API exists but not exposed |
| **CPU GPU-pinned** | Yes | `cuMemHostRegister()` |
| **MPI** | TCP only | No native RDMA (Windows lacks OpenUCX) |
| **MPI RDMA** | No | Disabled on Windows (network limitation) |
| **MPI GPU-aware** | No | Requires RDMA support |

### Tested Configurations

**NVIDIA:**

- Driver: 528+
- GPU: RTX 3070
- Vulkan: 1.3+
- MPI: OpenMPI 4.1 (built with --disable-dlopen)

**Intel:**

- Driver: Xe DG2+ (Arc A-series)
- GPU: UHD
- Vulkan: 1.3+
- MPI: OpenMPI 4.1+

### Performance Impact

**Limitations:**

- MPI TCP mode: ~5-10x slower than RDMA (100 Mbps vs 1-10 Gbps)
- No hugepages: ~10-20% slower for large remote buffers
- NUMA API: ~5% slower on multi-socket systems

**Recommendation:** Use Windows for single-node development, Linux for production clusters.

### Known Issues

| Issue | Impact | Workaround |
| ----- | ------ | ---------- |
| Windows Defender MPI socket blocking | Occasional MPI failures | Add exceptions |
| OpenMPI 4.0 on Windows unreliable | Hangs | Use 4.1+ |
| Vulkan driver crashes (rare) | GPU allocation fails | Update driver |

---

## macOS (tbd)

### Architecture Differences

**Key Issue:** Available hardware. macOS uses **unified memory** (Metal). Vulkan support only via MoltenVK wrapper.

### Feature Matrix (TBD)

| Feature | Status | Notes |
| ------- | ------ | ----- |
| **Vulkan GPU** | No | macOS doesn't natively support Vulkan (uses Metal only) |
| **Metal GPU** | TBD | Requires alternative implementation |
| **External Memory** | TBD | Metal equivalent unknown |
| **Vulkan IPC** | No | No Vulkan support, uses unified memory and thus shared memory |
| **CPU malloc** | Yes (shared memory) | Standard libc |
| **CPU NUMA** | No | macOS doesn't expose NUMA topology |
| **CPU hugepages** | No | No equivalent to Linux hugepages |
| **CPU GPU-pinned** | Yes | Unified memory inherently "pinned" |
| **MPI** | Untested | OpenMPI available, but untested on macOS |
| **MPI RDMA** | No | Not available |

### Recommended Approach

#### Option 1: Metal Backend (Long-term)

- Implement separate Metal allocator alongside Vulkan
- Handle unified memory with shared memory
- Estimated effort: 3-4 weeks for Phase 2

#### Option 2: Documentation Only (Short-term)

- Note: macOS supports local CPU shared memory channels only

### Known Issues

| Issue | Impact |
| ----- | ------ |
| No Vulkan support | Use of Vulkan not necessary for project |
| No NUMA topology | CPU allocation strategy differs |

---

## Cross-Platform Compatibility

### Version Requirements

| Platform | Minimum | Tested | Recommended |
| -------- | ------- | ------ | ----------- |
| **Vulkan** | 1.2 | 1.3+ | 1.3.x |
| **Rust** | 1.56+ | 1.70+ | 1.75+ |
| **OpenMPI** | 4.0 | 4.1+ | 4.1.x |
| **MPICH** | 3.3 | 3.4+ | 4.0+ |

### Cargo Feature Flags

```toml
[features]
default = ["vulkan"]
vulkan = ["ash", "gpu-alloc"]
mpi = ["mpi"]           # Phase 4+
cuda-interop = []       # Phase 5+
opencl-interop = []     # Phase 5+
opengl-interop = []     # Phase 5+
```

### Building for Each Platform

**Linux and Windows:**

```bash
cargo build --all-features
cargo test --all-features
mpirun -n 2 cargo test --test e2e
```

---

## Interoperability Matrix

### GPU Memory Import (Phase 5+)

| Source | Linux | Windows | macOS |
| ------ | ----- | ------- | ----- |
| **Vulkan->CUDA** | Yes | Yes | No |
| **Vulkan->OpenCL** | Yes | Yes | No |
| **Vulkan->OpenGL** | Yes | Yes | No |
| **CUDA->Vulkan** | Yes | Yes | No |
| **OpenCL->Vulkan** | Yes | Yes | No |

### MPI Transport Selection (Phase 4+)

| Platform | Preferred | Fallback | Status |
| -------- | --------- | -------- | ------ |
| **Linux** | RDMA (UCX) | TCP | Implemented |
| **Windows** | TCP | None | Phase 4 |
| **macOS** | TCP | None | TBD |

---

## Container Support

### Docker on Linux

**Vulkan GPU:**

```dockerfile
FROM nvidia/cuda:12.1-runtime-ubuntu22.04
RUN apt-get install -y vulkan-tools libvulkan-dev
# lava-flow works normally
```

**Scope Detection:** Hostname detection works across containers.

### Docker on Windows

**Not Recommended:** GPU passthrough complex, TCP MPI slow.

### Kubernetes

**GPU Support:** GPUs visible to containers.

**Scope Detection:** Pod hostname detection (pod-node awareness) not yet implemented.

---

## Performance by Platform

### Latency (< 100 ns target for Vulkan, ~10 us for MPI)

| Path | Linux | Windows | macOS |
| ---- | ----- | ------- | ----- |
| **GPU IPC** | < 100 ns | < 100 ns | N/A |
| **MPI RDMA** | ~1 us | N/A | N/A |
| **MPI TCP** | ~10 us | ~50 us | ~50 us |

### Estimated Throughput

| Path | Linux | Windows | macOS |
| ---- | ----- | ------- | ----- |
| **GPU IPC** | 100-900 GB/s | 100-900 GB/s | N/A |
| **MPI RDMA** | 100+ GB/s | N/A | N/A |
| **MPI TCP** | 1-10 GB/s | 100 Mbps | 100 Mbps |

---

## Recommendation Matrix

| Use Case | Recommended | Secondary | Not Supported |
| -------- | ----------- | --------- | ------------- |
| **GPU cluster (production)** | Linux + NVIDIA | Linux + AMD | macOS |
| **GPU development (local)** | WSL | Windows | macOS |
| **CPU-only HPC** | Linux | Windows | macOS (local CPU only) |
