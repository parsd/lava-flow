# Testing Strategy

## TL;DR

Three-tier testing (unit -> integration -> E2E) with MPI mocking for single-machine remote tests, CI/CD matrix for
Linux/Windows, and performance monitoring that accounts for jitter and shared runners.

## Testing Philosophy

**Goals:**

- Catch regressions early (CI runs all tests)
- Test remote without a cluster (mock MPI)
- Validate performance targets with jitter-aware checks
- Document expected behavior (tests as spec)
- Enable parallel development (independent layer testing)

## Three-Tier Testing Model

### Tier 1: Unit Tests

**Scope:** Single module, no I/O, no MPI, fast execution

**What to test:**

- Memory allocation (GPU and CPU)
- Scope detection logic
- Error cases and fallbacks
- Type conversions

**Location:** inline `#[cfg(test)]`

**Example:**

```rust
#[test]
fn test_scope_detection_same_host() {
    let scope = detect_scope(
        &ProcessLocation::localhost("gpu-node-0"),
        &ProcessLocation::localhost("gpu-node-0"),
    );
    assert_eq!(scope, CommunicationScope::Local);
}

#[test]
fn test_scope_detection_different_host() {
    let scope = detect_scope(
        &ProcessLocation::localhost("host-a"),
        &ProcessLocation::with_hostname("host-b", "gpu-0"),
    );
    assert_eq!(scope, CommunicationScope::Remote);
}
```

---

### Tier 2: Integration Tests

**Scope:** Module interaction, filesystem, local IPC (not network)

**What to test:**

- GPU memory export/import (Vulkan IPC)
- Channel creation and basic send/recv
- NUMA allocation correctness
- Hugepage allocation on Linux
- CPU memory sharing between processes

**Location:** `tests/integration/`

**Example:**

```rust
#[test]
fn test_gpu_memory_export_import() {
    // 1. Process A allocates GPU memory
    let allocator = MemoryAllocator::new();
    let gpu_buffer = allocator.allocate(
        1_000_000,
        MemoryLocation::GpuVulkan { device_id: 0 }
    ).expect("allocate GPU");

    // 2. Export handle
    let handle = gpu_buffer.export_handle().expect("export");

    // 3. Process B imports (simulate via fork or spawn)
    let imported = MemoryBuffer::import(handle).expect("import");

    // 4. Verify same memory
    assert_eq!(gpu_buffer.as_ptr(), imported.as_ptr());
}
```

**Multi-Process Testing:**

```rust
#[test]
fn test_channel_gpu_communication() {
    let (tx, rx) = std::process::Command::new(std::env::current_exe()?)
        .arg("--test-producer")
        .stdout(Stdio::pipe())
        .spawn_pair()?;  // Custom helper

    // Producer (in separate process)
    let buffer = allocator.allocate_auto(...)?;
    channel.send(buffer)?;

    // Consumer (in main process)
    let received = channel.recv()?;
    assert_eq!(received.len(), buffer.len());
}
```

---

### Tier 3: End-to-End Tests

**Scope:** Full system, multiple processes, MPI

**What to test:**

- Remote GPU communication (if available)
- MPI transport reliability
- Message ordering across nodes
- Failure recovery

**Location:** `tests/e2e/`

**Local E2E Options:**

- Use `mpirun` locally with multiple processes
- Use mock MPI for functional ordering checks
- Use a Docker-based local cluster for multi-host simulation

**Run with:** `mpirun -n 2 cargo test --test e2e`

**Example:**

```rust
#[test]
fn test_mpi_message_ordering() {
    let rank = mpi::rank();
    let size = mpi::size();

    if rank == 0 {
        for i in 0..1000 {
            channel.send(Frame::new(i))?;
        }
    } else {
        for i in 0..1000 {
            let frame = channel.recv()?;
            assert_eq!(frame.id(), i);  // Must be in order
        }
    }
}
```

---

## MPI Mocking for Single-Machine Tests

**Problem:** Testing remote on a single machine requires:

1. Multiple processes
2. MPI environment (`mpirun`)
3. Cluster setup

**Solution:** Mock MPI for local testing

### Mock MPI Implementation

```rust
#[cfg(test)]
mod mock_mpi {
    use std::sync::mpsc;
    use std::thread;

    pub struct MockMpiContext {
        rank: i32,
        size: i32,
        send_channels: Vec<mpsc::Sender<Vec<u8>>>,
        recv_channel: mpsc::Receiver<Vec<u8>>,
    }

    impl MockMpiContext {
        pub fn new(rank: i32, size: i32) -> Self {
            // Create ring of channels
            let mut send_channels = Vec::new();
            for _ in 0..size {
                let (tx, _rx) = mpsc::channel();
                send_channels.push(tx);
            }
            // Real recv_channel for this rank
            let (_tx, recv_channel) = mpsc::channel();

            Self { rank, size, send_channels, recv_channel }
        }

        pub fn send(&self, dest: i32, data: &[u8]) {
            self.send_channels[dest as usize]
                .send(data.to_vec())
                .expect("send");
        }

        pub fn recv(&self) -> Vec<u8> {
            self.recv_channel.recv().expect("recv")
        }
    }
}
```

### Usage in Tests

```rust
#[test]
fn test_inter_node_communication_mocked() {
    // Simulate 2-process MPI without actual mpirun
    let rank0 = MockMpiContext::new(0, 2);
    let rank1 = MockMpiContext::new(1, 2);

    // Rank 0 sends, Rank 1 receives
    rank0.send(1, b"hello");
    let msg = rank1.recv();
    assert_eq!(msg, b"hello");
}
```

---

## Performance Testing

### Feasibility Notes

Performance tests are sensitive to jitter (CPU scheduling, GPU clocks, shared runners). Use them as trend checks,
not strict pass/fail in CI unless running on stable, dedicated hardware.

### Tier 1: Latency Benchmarks

**Target (developer workstation):** < 100 ns for Vulkan IPC, ~10 us for MPI

**Setup:**

```rust
#[bench]
fn bench_gpu_message_latency(b: &mut Bencher) {
    let allocator = MemoryAllocator::new();
    let buffer = allocator.allocate_auto(...).expect("alloc");
    let channel = Channel::new(&allocator, &my_loc, &peer_loc).expect("channel");

    b.iter(|| {
        channel.send(&buffer).expect("send");
    });
}
```

**Acceptance Criteria (local):**

- Vulkan IPC: < 200 ns (allows 2x overhead)
- MPI: < 20 us (allows 2x overhead)

**Acceptance Criteria (CI):**

- Record results but do not hard-fail unless on dedicated runners

### Tier 2: Throughput Benchmarks

**Target (developer workstation):** 100-900 GB/s for GPU, 1-100 GB/s for MPI

**Setup:**

```rust
#[bench]
fn bench_gpu_throughput(b: &mut Bencher) {
    let buffer_size = 1_000_000_000; // 1 GB
    let buffer = allocator.allocate(
        buffer_size,
        MemoryLocation::GpuVulkan { device_id: 0 }
    )?;

    b.iter(|| {
        channel.send(&buffer).expect("send");
    });

    // Measure: time per GB
    let gb_per_sec = (buffer_size as f64) / elapsed_seconds / 1e9;
    assert!(gb_per_sec > 100.0, "Expected > 100 GB/s, got {}", gb_per_sec);
}
```

### Tier 3: Regression Detection

**CI Strategy:**

- Store benchmarks as artifacts and plot trends
- Compare only on consistent hardware (self-hosted runners)
- Allow wider thresholds in shared CI (e.g., 20-30%)
- Prefer median-of-N runs to reduce noise

---

## Doctests for API Boundaries

Doctests are useful for public API contracts, but should be scoped to avoid flaky GPU/IPC/MPI setup.

**What to cover:**

- Public types, builders, and configuration flows
- Small, cross-platform examples that compile everywhere

**What to avoid or gate:**

- GPU/Vulkan/MPI paths that need system dependencies
- Examples that depend on timing or external services

**Guidance:**

- Use `no_run` when setup is heavy but the example should compile
- Use `ignore` for platform-specific examples
- Feature-gate examples that require optional dependencies

---

## CI/CD Pipeline

### GitHub Actions Matrix

```yaml
name: Test
on: [push, pull_request]

jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
        rust: [stable, nightly]

    steps:
      - uses: actions/checkout@v2
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: ${{ matrix.rust }}

      # Tier 1: Unit tests
      - run: cargo test --lib

      # Tier 2: Integration tests
      - run: cargo test --test '*'

      # Tier 3: E2E (Linux only, requires OpenMPI)
      - if: matrix.os == 'ubuntu-latest'
        run: |
          sudo apt-get install -y libopenmpi-dev
          mpirun -n 2 cargo test --test e2e

      # Benchmarks (nightly only)
      - if: matrix.rust == 'nightly'
        run: cargo bench --no-run
```

### Performance Regression Detection

```yaml
  benchmark:
    runs-on: ubuntu-latest
    if: github.event_name == 'push' && github.ref == 'refs/heads/main'

    steps:
      - uses: actions/checkout@v2
      - run: cargo bench --bench gpu_latency -- --output-format bencher
      - uses: benchmark-action/github-action@v1
        with:
          tool: 'cargo'
          output-file-path: output.txt
          # Prefer warnings unless on dedicated runners
          comment-on-alert: true
          fail-on-alert: false
          alert-threshold: '130%'
```

---

## Test Hardware Matrix

| Component | Linux (required) | Windows | macOS |
| --------- | ---------------- | ------- | ----- |
| GPU allocation (Vulkan) | NVIDIA/AMD | NVIDIA/Intel | Limited |
| CPU allocation | All 4 | malloc only | malloc |
| Vulkan IPC | Full | Full | TBD |
| MPI | Full (OpenMPI) | TCP mode | TBD |
| RDMA | Yes (UCX) | Disabled | No |

---

## Test File Organization

```text
tests/
+-- unit/                           # Tier 1: Unit tests
|   +-- test_scope_detection.rs
|   +-- test_gpu_memory.rs
|   +-- test_cpu_memory.rs
|   +-- test_error_handling.rs
|
+-- integration/                    # Tier 2: Integration tests
|   +-- test_gpu_ipc.rs
|   +-- test_channel_basic.rs
|   +-- test_numa_allocation.rs
|   +-- test_cpu_shared_memory.rs
|
+-- e2e/                            # Tier 3: End-to-End Tests
|   +-- test_mpi_communication.rs
|   +-- test_multi_node.rs
|
+-- mock_mpi.rs                     # MPI mocking for Tier 2
+-- common.rs                       # shared test utilities
```

---

## Test Execution Guide

### One-Time Setup

```bash
# Install coverage tool
cargo install cargo-llvm-cov
```

### Local Development

```bash
# Tier 1 only (fast, no dependencies)
cargo test --lib

# Tier 1 + 2 (requires Vulkan SDK)
cargo test

# Tier 2 specific
cargo test --test integration

# Benchmarks (best-effort, local only)
cargo bench

# Coverage (workspace)
cargo llvm-cov --workspace --all-features

# Coverage HTML report
cargo llvm-cov --workspace --all-features --html
# open: target/llvm-cov/html/index.html
```

### Before Commit

```bash
# All unit + integration
cargo test --lib
cargo test --test '*'

# Check for regressions (local)
cargo bench --no-run
```

### CI/CD (Full)

```bash
# All three tiers
cargo test --all
mpirun -n 2 cargo test --test e2e
cargo bench --bench '*'
```

---

## Test Coverage Goals

| Layer | Unit | Integration | E2E | Coverage |
| ----- | ---- | ----------- | --- | -------- |
| **Layer 1 (Memory)** | 90%+ | 80%+ | N/A | High |
| **Layer 2 (Channels)** | 85%+ | 75%+ | 60%+ | Medium |
| **Error handling** | 100% | 100% | 100% | Critical |
| **Scope detection** | 100% | 100% | 100% | Critical |

---

## Known Limitations

1. **macOS Vulkan:** Limited support (Metal, not Vulkan), testing TBD
1. **Windows MPI:** TCP mode only (no RDMA), slower remote tests
1. **Docker:** Container hostname detection untested
1. **Kubernetes:** Pod-aware detection not yet implemented

---

## References

- [MPI Standard](https://www.mpi-forum.org/docs/)
- [OpenMPI Documentation](https://www.open-mpi.org/doc/)
- [Vulkan External Memory](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory.html)
- [Rust Criterion](https://bheisler.github.io/criterion.rs/book/) micro benchmarking
