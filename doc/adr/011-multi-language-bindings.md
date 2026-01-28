# ADR-011: Multi-Language Bindings

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

Language bindings (C++, Python) are **orthogonal** to architectural layers (Memory, Channels). Rust is core, FFI
wrappers added in Phase 6+ after core stable.

## Decision

**Language bindings are orthogonal, not a separate layer.**

## Rationale

* **Architecture layers:** Data flow (Memory -> Channels)
* **Language bindings:** API access (Rust -> C++ and Rust -> Python)

These are independent concerns:

* Can add Python without changing architecture
* Can refactor Layer 1 without breaking C++ API
* Languages share same core (no duplication)

## Implementation

**Rust core (Phase 1-5):**

```rust
pub struct MemoryAllocator { ... }
impl MemoryAllocator {
    pub fn allocate(...) -> Result<Buffer, Error> { ... }
}
```

**C++ FFI (Phase 6+):**

```cpp
auto allocator = lavaflow::Allocator{};
std::unique_ptr<lavaflow::Buffer> buffer = allocator.allocate(1000 * 1000);
```

**Python (Phase 6+):**

```python
import lavaflow

allocator = lavaflow.Allocator()
buffer: lavaflow.Buffer = allocator.allocate(1_000_000)
```

## Consequences

* **Orthogonal** — Language support independent of architecture
* **Single core** — No code duplication
* **Deferred** — Phase 6+ (after core stable)
