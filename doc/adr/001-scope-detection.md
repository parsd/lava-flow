# ADR-001: Scope Detection Strategies

**Status:** Accepted | **Date:** 2025-12-27 | **Supersedes:** None

## TL;DR

Automatically detect whether target process is on same machine or remote using multiple fallback strategies (hostname ->
MPI -> container metadata -> conservative default). Default is fully automatic, with a test-only override to force MPI
in Phase 4+.

## Problem

GPU communication backends have vastly different characteristics:

- **local:** < 100 ns latency possible via Vulkan external memory (shared GPU VRAM)
- **remote:** ~10 us latency via MPI messaging over network
- **Wrong choice:** Vulkan IPC across network fails, MPI on same node wastes 100x latency

Manual backend selection creates:

- Configuration burden (user must know topology)
- Silent failures (misconfigured, crashes)
- Code duplication (if-else per deployment)
- Migration friction (changing cluster requires code changes)

Real-world scenarios to handle:

- Same hostname, different containers (Docker/Kubernetes)
- DNS failures (hostname resolution broken)
- MPI batch systems (location from MPI runtime)
- Heterogeneous systems (some nodes have GPUs, some don't)

## Decision

**Implement automatic scope detection with multi-strategy fallback:**

```text
Strategy 1: Hostname Comparison (Primary)
├─ hostname::get() on localhost
├─ Compare with peer ProcessLocation.hostname
└─ If same → Local, else → Remote

Strategy 2: MPI Runtime Detection (Phase 4+)
├─ If MPI initialized: MPI_Get_processor_name()
├─ Compare MPI hostnames
└─ If same → Local, else → Remote

Strategy 3: Container-Aware (Future)
├─ Check Kubernetes metadata (pod.spec.nodeName)
├─ Check Docker hostname vs container hostname
└─ Resolve to physical node

Strategy 4: Conservative Fallback
└─ Assume Remote (safer, never breaks)
```

Users provide `ProcessLocation { hostname, node_id, device_id }`, library selects transport automatically.

In any case the chosen tech stack can be queried so that user can check/log to debug performance issues easily. For
Phase 4 testing, allow an explicit override to force MPI even on same-host processes.

## Alternatives Considered

### Alternative 1: Explicit Backend Selection

```rust
// User picks backend
let backend = if same_machine {
    Backend::VulkanIPC
} else {
    Backend::MpiPointToPoint
};
let channel = Channel::with_backend(backend, ...)?;
```

**Pros:**

- Maximum control
- Simple implementation (no detection logic)
- No runtime overhead

**Cons:**

- **Fatal:** Wrong choice doesn't work or produces error (Vulkan IPC over network fails silently)
- Duplicates topology info (user must re-encode what scheduler knows)
- Violates DRY (topology duplicated in config + code)
- Can't migrate deployment without code change
- Users need deep hardware knowledge (which GPUs on which nodes?)
- Testing harder (must test both code paths explicitly)

**Rejected:** Remove maintenance burden and likely user errors.

---

### Alternative 2: Compile-Time Configuration

```rust
#[cfg(feature = "single-node")]
type Backend = VulkanIpc;

#[cfg(feature = "cluster")]
type Backend = MpiPointToPoint;
```

**Pros:**

- Zero runtime overhead
- Very simple (no detection logic)

**Cons:**

- **Fatal:** Need recompilation for different deployments
- Can't mix on same machine (e.g., testing single-node then cluster)
- Inflexible for heterogeneous systems (some nodes single-GPU, some multi-host deployments)
- CI/CD harder (multiple builds per deployment)

**Rejected:** HPC workflows often test locally before cluster deployment. Recompilation creates friction and added
complexity.

---

### Alternative 3: Environment Variable

```rust
let backend = match env::var("LAVA_FLOW_BACKEND")? {
    "vulkan" => Backend::VulkanIpc,
    "mpi" => Backend::MpiPointToPoint,
    _ => panic!("Invalid backend"),
};
```

**Pros:**

- No recompilation
- Easy to override for testing

**Cons:**

- Still manual configuration (defeats goal)
- Silent failures if misconfigured
- Harder to debug (environment pollution)

**Rejected:** Configuration burden remains for normal usage. However, a restricted, test-only override is acceptable to
validate MPI in Phase 4+.

---

### Alternative 4: Automatic Scope Detection **CHOSEN**

```rust
let scope = CommunicationScope::from_locations(&me, &peer);
// Library internally picks optimal path:
// Local → Vulkan IPC (< 100 ns)
// Remote → MPI (~ 10 us)
```

**Pros:**

- **Zero user configuration** — Works automatically
- **Single codebase** — Same binary works on laptop, single-node server, HPC cluster
- **Optimal performance** — Always picks fastest transport for topology
- **No recompilation** — Deploy anywhere without rebuilding
- **Room for error eliminated** — Can't pick wrong backend
- **Testing easier** — Same code path, different inputs

**Cons:**

- Slight runtime overhead (detection happens once at channel creation, ~1 ms)
- Requires location metadata (hostname, node_id)
- Edge cases need fallback strategies

**Chosen:** Benefits vastly outweigh costs. Detection is one-time (not per-message).

## Implementation

### Data Structure

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessLocation {
    pub hostname: String,        // e.g., "gpu-node-0"
    pub node_id: Option<u32>,    // e.g., Some(0)
    pub device_id: Option<u32>,  // e.g., Some(0) for first GPU
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CommunicationScope {
    Local,  // Same hostname
    Remote, // Different hostname
}
```

### Detection Logic (Strategy Priority)

```rust
pub fn detect_scope(
    my_location: &ProcessLocation,
    peer_location: &ProcessLocation,
) -> CommunicationScope {
    // Test-only override (Phase 4+) to force MPI path
    if let Some(scope) = scope_override_from_env() {
        return scope;
    }
    // Strategy 1: Hostname comparison (always works)
    if my_location.hostname == peer_location.hostname {
        return CommunicationScope::Local;
    }

    // Strategy 2: MPI runtime (Phase 4+, if MPI initialized)
    #[cfg(feature = "mpi")]
    if let Some(mpi_scope) = detect_scope_via_mpi(my_location, peer_location) {
        return mpi_scope;
    }

    // Strategy 3: Container-aware (future)
    // TODO: Check Kubernetes pod.spec.nodeName
    // TODO: Check Docker hostname resolution

    // Strategy 4: Conservative fallback
    CommunicationScope::Remote
}
// Example only; gated to test builds or explicit debug flag
fn scope_override_from_env() -> Option<CommunicationScope> {
    // e.g., LAVA_FLOW_SCOPE_OVERRIDE=remote
    None
}
```

### Edge Cases

#### Case 1: Same hostname, different containers

```text
Container A: hostname = "worker-0" (Docker assigns same hostname)
Container B: hostname = "worker-0"
Physical nodes: Different
```

**Solution:** Future Strategy 3 (container metadata). For now, falls back to Remote (safer).

#### Case 2: DNS failure

```text
my_location.hostname = "gpu-node-0"
peer_location.hostname = unresolvable
```

**Solution:** String comparison works without DNS. If user provides IPs instead of hostnames, document as unsupported
(use hostnames).

#### Case 3: MPI batch system

```text
mpirun -n 2 ./program
# MPI knows topology, user doesn't provide ProcessLocation
```

**Solution:** Strategy 2 (Phase 4+): Use `MPI_Get_processor_name()` automatically.

### Performance

**One-time cost:** Detection happens at `Channel::create()`, not per message.

```rust
// Happens once:
let channel = Channel::create(&allocator, &my_loc, &peer_loc)?;
// ↑ ~1 ms (hostname comparison + string alloc)

// Happens many times:
channel.send(frame)?;  // No detection overhead
```

**Amortized:** Detection cost is 0.001% if sending 1000+ messages and might not be in the performance critical path of
the application.

## Consequences

### Positive

- **Single codebase for all deployments** — Laptop, single-node, cluster use same binary
- **No user configuration errors** — Library always picks optimal transport
- **Optimal performance automatically** — < 100 ns local, ~10 us remote
- **Easy testing** — Test both paths by changing ProcessLocation inputs
- **Migration-friendly** — Move from single-node to cluster without code change
- **Future-proof** — Can add Strategy 3 (container detection) without API change

### Negative

- **Requires location metadata** — Users must provide `ProcessLocation` (hostname at minimum)
- **Runtime detection overhead** — ~1 ms per channel creation (negligible for long-lived channels)
- **No manual override by default** - Users can't force MPI for node-local
- **Container edge cases** — Same hostname in containers needs Strategy 3 (future work)

### Trade-offs Accepted

- **Metadata requirement:** Better than misconfiguration risk
- **Detection overhead:** 1 ms once vs 100x latency savings per message
- **No override by default:** Prevents user error (forced wrong backend would result in errors)
- **Test-only override:** Needed for Phase 4 MPI validation without exposing a user-facing config knob.

## Testing

```rust
#[test]
fn test_same_hostname_intra_node() {
    let scope = detect_scope(
        &ProcessLocation::new("gpu-node-0", Some(0), Some(0)),
        &ProcessLocation::new("gpu-node-0", Some(0), Some(1)),
    );
    assert_eq!(scope, CommunicationScope::Local);
}

#[test]
fn test_different_hostname_multi_node() {
    let scope = detect_scope(
        &ProcessLocation::new("gpu-node-0", Some(0), Some(0)),
        &ProcessLocation::new("gpu-node-1", Some(1), Some(0)),
    );
    assert_eq!(scope, CommunicationScope::Remote);
}

#[test]
fn test_empty_hostname_fallback() {
    let scope = detect_scope(
        &ProcessLocation::new("", None, None),
        &ProcessLocation::new("", None, None),
    );
    // Empty hostname → conservative fallback
    assert_eq!(scope, CommunicationScope::Remote);
}
```

---

**Decision:** Automatic scope detection with hostname comparison (Strategy 1) as primary, MPI runtime (Strategy 2) in
Phase 4+, container-aware (Strategy 3) as future work, conservative fallback (Strategy 4) for edge cases.

**Rationale:** Single codebase works everywhere without recompilation or configuration. Performance cost negligible
(1ms once vs 100x per-message savings).
