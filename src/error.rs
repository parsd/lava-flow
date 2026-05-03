use thiserror::Error;

use crate::types::CommunicationScope;

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

    /// Vulkan backend operation failed.
    #[error("vulkan operation failed during {operation}: {details}")]
    VulkanOperation {
        /// Vulkan operation name.
        operation: &'static str,
        /// Human-readable details, typically a Vulkan result code.
        details: String,
    },

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

    /// Channel metadata serialization or deserialization failed.
    #[error("channel metadata codec failed during {operation}")]
    ChannelMetadataCodec {
        /// Metadata codec operation name.
        operation: &'static str,
        /// Source serialization error.
        #[source]
        source: serde_json::Error,
    },

    /// Requested channel metadata encoding is not implemented.
    #[error("unsupported metadata encoding: {encoding}")]
    UnsupportedMetadataEncoding {
        /// Human-readable encoding name.
        encoding: &'static str,
    },

    /// Channel transport I/O failed during a platform operation.
    #[error("channel transport operation failed during {operation}")]
    ChannelTransportOperation {
        /// Transport operation name.
        operation: &'static str,
        /// Source I/O error.
        #[source]
        source: std::io::Error,
    },

    /// Channel transport was disconnected before the operation completed.
    #[error("channel transport disconnected")]
    ChannelDisconnected,

    /// Blocking channel endpoint construction was cancelled before it completed.
    #[error("channel build cancelled for {endpoint} endpoint")]
    ChannelBuildCancelled {
        /// Endpoint kind whose build was cancelled.
        endpoint: &'static str,
    },

    /// The requested communication scope is not implemented by the current channel runtime.
    #[error("unsupported communication scope: {scope:?}")]
    UnsupportedCommunicationScope {
        /// Requested communication scope.
        scope: CommunicationScope,
    },

    /// Channel buffer kind is not supported by the selected transport.
    #[error("unsupported channel buffer kind: {kind}")]
    UnsupportedChannelBufferKind {
        /// Buffer kind string for diagnostics.
        kind: &'static str,
    },
}

/// Standard result type for lava-flow APIs.
pub type Result<T> = std::result::Result<T, LavaFlowError>;
