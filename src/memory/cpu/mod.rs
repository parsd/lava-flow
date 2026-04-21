use crate::error::{AllocationReason, LavaFlowError, Result};
use crate::memory::allocator::InterprocessMemoryHandle;
use std::env;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix::SharedMemoryRegion;
#[cfg(windows)]
use windows::SharedMemoryRegion;

/// Stateless API wrapper around CPU memory allocation strategies.
#[derive(Debug)]
pub struct Allocator {
    max_allocation_size: usize,
}

impl Allocator {
    /// Creates a new allocator configured from environment.
    ///
    /// Uses `LAVA_FLOW_MAX_CPU_ALLOCATION_SIZE` when valid and non-zero.
    pub fn new() -> Self {
        let hard = hard_max_cpu_allocation_size();
        let raw = env::var("LAVA_FLOW_MAX_CPU_ALLOCATION_SIZE").ok();
        let max_allocation_size = parse_cpu_allocation_cap(raw.as_deref(), hard);
        Self {
            max_allocation_size,
        }
    }

    /// Creates a new allocator with an explicit allocation cap.
    ///
    /// A value of `0` means "use the platform hard limit".
    pub fn with_max_allocation_size(max_allocation_size: usize) -> Self {
        let hard = hard_max_cpu_allocation_size();
        let capped = if max_allocation_size == 0 {
            hard
        } else {
            max_allocation_size.min(hard)
        };
        Self {
            max_allocation_size: capped,
        }
    }

    /// Returns the effective maximum CPU allocation size in bytes.
    pub fn max_allocation_size(&self) -> usize {
        self.max_allocation_size
    }

    /// Allocates standard host memory.
    pub fn allocate(&self, size: usize) -> Result<MemoryBuffer> {
        let region = SharedMemoryRegion::create(size, self.max_allocation_size)?;
        let mut buffer = MemoryBuffer { region };
        buffer.zero_fill();
        Ok(buffer)
    }
}

impl Default for Allocator {
    fn default() -> Self {
        Self::new()
    }
}

/// CPU-side allocation strategy.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum AllocationStrategy {
    /// Default host allocation.
    Standard,
    /// Host memory that is page-locked/pinned.
    GpuPinned,
}

/// CPU-backed memory buffer metadata and storage.
#[derive(Debug)]
pub struct MemoryBuffer {
    region: SharedMemoryRegion,
}

impl MemoryBuffer {
    /// Imports a CPU shared-memory handle into a buffer with platform hard-limit validation.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_shared_handle(
        size: usize,
        handle: InterprocessMemoryHandle,
    ) -> Result<Self> {
        let region = SharedMemoryRegion::from_handle(size, hard_max_cpu_allocation_size(), handle)?;
        Ok(Self { region })
    }

    fn zero_fill(&mut self) {
        unsafe { std::ptr::write_bytes(self.as_mut_ptr(), 0, self.size()) };
    }

    /// Returns an immutable byte slice view over the whole allocation.
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.as_ptr(), self.size()) }
    }

    /// Returns a mutable byte slice view over the whole allocation.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.as_mut_ptr(), self.size()) }
    }

    /// Returns an immutable raw pointer to the beginning of the buffer.
    pub fn as_ptr(&self) -> *const u8 {
        self.region.as_ptr()
    }

    /// Returns a mutable raw pointer to the beginning of the buffer.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.region.as_mut_ptr()
    }

    /// Returns the buffer size in bytes.
    pub fn size(&self) -> usize {
        self.region.size()
    }

    /// Returns the allocation strategy used to create this buffer.
    pub fn strategy(&self) -> AllocationStrategy {
        AllocationStrategy::Standard
    }

    /// Returns the interprocess shared-memory handle for this CPU buffer.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn shared_handle(&self) -> Result<InterprocessMemoryHandle> {
        self.region.export_handle()
    }
}

fn hard_max_cpu_allocation_size() -> usize {
    // Keep allocations within platform addressability limits and (on Unix) ftruncate/off_t range.
    #[cfg(unix)]
    {
        let off_t_max = libc::off_t::MAX as u128;
        let pointer_max = isize::MAX as u128;
        let cap = off_t_max.min(pointer_max);
        usize::try_from(cap).unwrap_or(isize::MAX as usize)
    }
    #[cfg(not(unix))]
    {
        isize::MAX as usize
    }
}

fn parse_cpu_allocation_cap(raw: Option<&str>, hard: usize) -> usize {
    let configured = match raw {
        Some(value) => match value.parse::<usize>() {
            Ok(parsed) if parsed > 0 => parsed,
            _ => hard,
        },
        None => hard,
    };
    configured.min(hard)
}

fn validate_size(size: usize, max_allocation_size: usize) -> Result<()> {
    if size == 0 {
        return Err(LavaFlowError::InvalidAllocationRequest {
            size,
            reason: AllocationReason::ZeroSize,
        });
    }
    if size > max_allocation_size {
        return Err(LavaFlowError::InvalidAllocationRequest {
            size,
            reason: AllocationReason::ExceedsMaxSize,
        });
    }
    Ok(())
}

fn shared_memory_error(operation: &'static str) -> LavaFlowError {
    let source = std::io::Error::last_os_error();
    let source = if source.raw_os_error().is_none() || source.raw_os_error() == Some(0) {
        std::io::Error::other(format!("{operation} failed"))
    } else {
        source
    };
    LavaFlowError::SharedMemoryOperation { operation, source }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::env::Guard as EnvGuard;

    const BUFFER_SIZE: usize = 64;
    const SMALL_CAP: usize = 64;
    const OVER_CAP_SIZE: usize = 65;
    const TEST_BYTE_OFFSET: usize = 3;
    const TEST_BYTE_VALUE: u8 = 0x7f;
    const ENV_MAX_CPU_ALLOCATION_SIZE: &str = "LAVA_FLOW_MAX_CPU_ALLOCATION_SIZE";
    const TEST_HARD_CAP: usize = usize::MAX;
    const ENV_CONFIGURED_CAP: usize = 1024;

    fn test_allocator() -> Allocator {
        Allocator::with_max_allocation_size(TEST_HARD_CAP)
    }

    #[test]
    fn standard_allocation_exposes_mutable_and_immutable_views() {
        let allocator = test_allocator();
        let mut buffer = allocator.allocate(BUFFER_SIZE).expect("allocate standard");
        assert_eq!(buffer.size(), BUFFER_SIZE);
        assert!(!buffer.as_ptr().is_null());
        assert!(!buffer.as_mut_ptr().is_null());
        assert_eq!(buffer.strategy(), AllocationStrategy::Standard);
        assert_eq!(buffer.as_slice().len(), BUFFER_SIZE);
        assert_eq!(buffer.as_mut_slice().len(), BUFFER_SIZE);
    }

    #[test]
    fn standard_allocation_is_zero_initialized() {
        let allocator = test_allocator();
        let buffer = allocator.allocate(BUFFER_SIZE).expect("allocate standard");
        assert!(buffer.as_slice().iter().all(|&b| b == 0));
    }

    #[test]
    fn imported_shared_handle_observes_written_bytes() {
        let allocator = test_allocator();
        let mut original = allocator.allocate(BUFFER_SIZE).expect("allocate original");
        original.as_mut_slice()[TEST_BYTE_OFFSET] = TEST_BYTE_VALUE;

        let handle = original.shared_handle().expect("export handle");
        let imported =
            MemoryBuffer::from_shared_handle(BUFFER_SIZE, handle).expect("import shared handle");
        assert_eq!(imported.as_slice()[TEST_BYTE_OFFSET], TEST_BYTE_VALUE);
    }

    #[test]
    fn configured_cap_is_enforced() {
        let allocator = Allocator::with_max_allocation_size(SMALL_CAP);
        let err = allocator
            .allocate(OVER_CAP_SIZE)
            .expect_err("allocation over configured cap must fail");
        assert!(matches!(
            err,
            LavaFlowError::InvalidAllocationRequest {
                size: OVER_CAP_SIZE,
                reason: AllocationReason::ExceedsMaxSize,
            }
        ));
    }

    #[test]
    fn zero_cap_configuration_uses_hard_limit() {
        assert_eq!(
            Allocator::with_max_allocation_size(0).max_allocation_size(),
            hard_max_cpu_allocation_size()
        );
    }

    #[test]
    fn parse_cpu_allocation_cap_accepts_valid_value() {
        assert_eq!(parse_cpu_allocation_cap(Some("128"), 1024), 128);
    }

    #[test]
    fn parse_cpu_allocation_cap_rejects_zero_and_invalid_values() {
        assert_eq!(parse_cpu_allocation_cap(Some("0"), 1024), 1024);
        assert_eq!(parse_cpu_allocation_cap(Some("invalid"), 1024), 1024);
        assert_eq!(parse_cpu_allocation_cap(None, 1024), 1024);
    }

    #[test]
    fn default_allocator_reads_env_cap() {
        let _guard = EnvGuard::set(ENV_MAX_CPU_ALLOCATION_SIZE, &ENV_CONFIGURED_CAP.to_string());
        let allocator = Allocator::default();
        assert_eq!(allocator.max_allocation_size(), ENV_CONFIGURED_CAP);
    }

    #[test]
    fn shared_memory_error_keeps_operation_name() {
        let err = shared_memory_error("unit_test");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation { operation, .. } if operation == "unit_test"
        ));
    }

    #[test]
    fn zero_size_allocation_is_rejected() {
        let allocator = test_allocator();
        let err = allocator
            .allocate(0)
            .expect_err("zero-sized allocation must fail");
        assert!(matches!(
            err,
            LavaFlowError::InvalidAllocationRequest {
                size,
                reason: AllocationReason::ZeroSize,
            } if size == 0
        ));
    }

    #[test]
    fn oversized_allocation_is_rejected() {
        let allocator = test_allocator();
        let max = allocator.max_allocation_size();
        let oversized = max
            .checked_add(1)
            .expect("allocation cap leaves room for overflow test");
        let err = allocator
            .allocate(oversized)
            .expect_err("oversized allocation must fail");
        assert!(matches!(
            err,
            LavaFlowError::InvalidAllocationRequest {
                size,
                reason: AllocationReason::ExceedsMaxSize,
            } if size == oversized
        ));
    }
}
