# ADR-004: Vulkan Version Requirement

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

Require Vulkan 1.2 minimum because `VK_KHR_external_memory` promoted to core feature. Vulkan 1.0/1.1 requires extension
querying (complex, error-prone).

## Problem

External memory support varies by Vulkan version:

- **1.0:** No external memory (needs `VK_KHR_external_memory` extension, not guaranteed)
- **1.1:** Extension promoted to core but still optional
- **1.2+:** External memory guaranteed core feature

Must choose minimum version balancing:

- **Compatibility:** Older drivers
- **Simplicity:** Avoid extension querying
- **Features:** External memory required

## Decision

**Minimum Vulkan version:** 1.2

## Rationale

### Vulkan 1.0 (Rejected)

**Pros:**

- Widest compatibility (2016+ drivers)

**Cons:**

- No external memory (extension not guaranteed)
- Complex extension querying required
- Many drivers don't support `VK_KHR_external_memory` on 1.0

### Vulkan 1.1 (Rejected)

**Pros:**

- External memory promoted to core
- Still good compatibility (2018+ drivers)

**Cons:**

- Extension still optional (devices can omit)
- Must check device features at runtime

### Vulkan 1.2 (Chosen)

**Pros:**

- External memory guaranteed core feature
- No extension querying needed
- Simpler code (no runtime checks)
- Modern driver support (2020+ drivers)

**Cons:**

- Older drivers unsupported (pre-2020)

**Trade-off:** Simplicity and reliability > maximum compatibility. 2020+ drivers reasonable requirement for 2026
library.

## Implementation

```rust
pub const MIN_VULKAN_VERSION: u32 = vk::make_api_version(0, 1, 2, 0);

pub fn check_vulkan_version(instance: &Instance) -> Result<()> {
    let props = instance.enumerate_instance_version()?;
    if props < MIN_VULKAN_VERSION {
        return Err(Error::VulkanVersionTooOld {
            required: "1.2",
            found: format_version(props),
        });
    }
    Ok(())
}
```

## Consequences

- **Simpler code** — No extension querying
- **Guaranteed features** — External memory always available
- **Driver requirement** — 2020+ drivers (NVIDIA 460+, AMD AMDGPU 5.x+, Intel Xe+)
