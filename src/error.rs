use thiserror::Error;

/// Shared validation reasons reused across public API errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ValidationReason {
    /// The provided value was empty.
    #[error("must not be empty")]
    Empty,

    /// The provided identifier exceeded the identifier length limit.
    #[error("must be at most 64 characters")]
    IdentifierTooLong,

    /// The provided hostname exceeded the hostname length limit.
    #[error("must be at most 253 characters")]
    HostnameTooLong,

    /// The first character was not allowed.
    #[error("must start with an ASCII letter or digit")]
    InvalidStartCharacter,

    /// One or more characters were outside the allowed identifier charset.
    #[error("must contain only lowercase ASCII letters, digits, '-' or '_'")]
    InvalidCharacters,
}

/// Shared reasons for allocation request failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AllocationReason {
    /// The requested size was zero.
    #[error("allocation size must be greater than zero")]
    ZeroSize,
    /// The requested size exceeded the current allocation limit.
    #[error("allocation size exceeds maximum supported size")]
    ExceedsMaxSize,
}

/// Top-level error type for lava-flow core APIs.
#[derive(Debug, Error)]
pub enum LavaFlowError {
    /// Process name validation failed.
    #[error("invalid process name `{value}`: {reason}")]
    InvalidProcessName {
        /// The original rejected input.
        value: String,
        /// Structured reason for rejection.
        reason: ValidationReason,
    },

    /// Channel ID validation failed.
    #[error("invalid channel id `{value}`: {reason}")]
    InvalidChannelId {
        /// The original rejected input.
        value: String,
        /// Structured reason for rejection.
        reason: ValidationReason,
    },

    /// Hostname validation failed.
    #[error("invalid hostname `{value}`: {reason}")]
    InvalidHostname {
        /// The original rejected input.
        value: String,
        /// Structured reason for rejection.
        reason: ValidationReason,
    },

    /// Hostname detection via OS APIs failed.
    #[error("failed to detect local hostname")]
    HostnameDetection(#[source] std::io::Error),

    /// Memory allocation request failed validation.
    #[error("invalid allocation request (size={size}): {reason}")]
    InvalidAllocationRequest {
        /// Requested allocation size in bytes.
        size: usize,
        /// Structured reason for rejection.
        reason: AllocationReason,
    },

    /// A requested GPU device id is not known by the allocator.
    #[error("GPU device `{device_id}` not found")]
    GpuDeviceNotFound {
        /// Requested device id.
        device_id: u32,
    },

    /// Interprocess handle kind is not supported for the requested operation.
    #[error("unsupported interprocess handle for operation: {kind}")]
    UnsupportedInterprocessHandle {
        /// Handle kind string for diagnostics.
        kind: &'static str,
    },

    /// GPU allocation was requested but no GPU backend is available.
    #[error("GPU backend is not available")]
    GpuBackendUnavailable,

    /// Internal allocator state lock was poisoned by a prior panic.
    #[error("allocator state lock poisoned: {component}")]
    AllocatorStatePoisoned {
        /// Internal component that failed lock acquisition.
        component: &'static str,
    },

    /// OS shared-memory operation failed.
    #[error("shared memory operation failed during {operation}")]
    SharedMemoryOperation {
        /// Shared-memory operation name.
        operation: &'static str,
        /// Source OS error.
        #[source]
        source: std::io::Error,
    },
}

/// Standard result type for lava-flow APIs.
pub type Result<T> = std::result::Result<T, LavaFlowError>;
