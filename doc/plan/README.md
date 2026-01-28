﻿# lava-flow Implementation Plan

## TL;DR

Six-phase roadmap over 15-18 person weeks. Each phase is independently testable and deliverable.

## Overview

| Phase | Duration | Focus | Deliverable | Dependencies |
| ----- | -------- | ----- | ----------- | ------------ |
| **1** | 1-2 weeks | Core types, scope detection | Working scope detector | None |
| **2** | 2-3 weeks | Memory allocators (GPU + CPU) | Unified memory API | Vulkan SDK 1.2+ |
| **3** | 2-3 weeks | Vulkan IPC channels | Local channels | Phase 1, 2 |
| **4** | 2-3 weeks | MPI transport | Remote channels | Phase 1, 2, OpenMPI 4.0+ |
| **5** | 1-2 weeks | Cross-API sync, semaphores | CUDA/OpenCL import | Phase 3, 4 |
| **6+** | 3+ weeks | Multi-language bindings | C++/Python APIs | Phase 1-5 stable |

**Total estimated time:** 15-18 weeks (MVP = Phases 1-4, 10-12 weeks)

---

## Phase Details

Detailed phase specs live in the following files:

- [Phase 1](phase_1.md) (core types and scope detection)
- [Phase 2](phase_2.md) (memory and allocator)
- [Phase 3](phase_3.md) (Vulkan IPC channels)
- [Phase 4](phase_4.md) (MPI transport)
- [Phase 5](phase_5.md) (cross-API sync)
- [Phase 6](phase_6.md) (multi-language bindings)

For decision rationale, see [ADRs](../adr/README.md).

## Risk Assessment

### High Risk (Must Address Before MVP)

| Risk | Impact | Mitigation |
| ---- | ------ | ---------- |
| **Vulkan driver bugs** | Crashes | Test on known-good drivers (NVIDIA 475+, AMD AMDGPU 5.x+), define versions |
| **RDMA over MPI unavailable on Windows** | No free RDMA on Windows | Document TCP-only limitation, prioritize Linux |
| **GPU out of memory** | Allocation fails | Proper error handling + CPU fallback |

### Medium Risk (Monitor)

| Risk | Impact | Mitigation |
| ---- | ------ | ---------- |
| **macOS Vulkan limited** | CPU or custom backend on macOS | Document limitation, defer to Phase 5+ (Metal backend) |
| **Container hostname same** | Wrong scope detection | Strategy 3 (container detection) in future, fallback works |
| **Performance targets missed** | Slower than expected | Profile early (Phase 3), optimize hot paths |

### Low Risk (Acceptable)

| Risk | Impact | Mitigation |
| ---- | ------ | ---------- |
| **Windows hugepages unavailable** | 10-20% slower remote | Document limitation, acceptable for Tier 2 platform |
| **Older drivers unsupported** | Limited compatibility | Reasonable requirement (2020+ drivers for 2026 library) |

---

## Success Metrics

Phase-level success criteria are defined in the detailed phase specs.

## Timeline

| Time Since Start | Phase |
| ---------------- | ----- |
| Week 1-2 | Phase 1 (Core Types) |
| Week 3-5 | Phase 2 (Memory & Allocator) |
| Week 6-8 | Phase 3 (Vulkan IPC) |
| Week 9-11 | Phase 4 (MPI Transport) |
| Week 12-13 | Phase 5 (Cross-API) [optional for MVP] |
| Week 14+ | Phase 6+ (Multi-Language) [optional for MVP] |

**MVP:** Week 11 (Phases 1-4)

**Full:** Week 17+ (Phases 1-6)

---

## Next Steps

1. **Review Phase 1 spec** [Phase 1](phase_1.md)
2. **Set up development environment** (Rust 1.70+, Vulkan SDK 1.2+)
3. **Start Phase 1 implementation** (core types)
4. **Weekly progress reviews** (adjust timeline as needed)
