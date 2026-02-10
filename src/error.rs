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
}

/// Standard result type for lava-flow APIs.
pub type Result<T> = std::result::Result<T, LavaFlowError>;
