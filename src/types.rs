use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{LavaFlowError, Result, ValidationReason};

const MAX_IDENTIFIER_LEN: usize = 64;
const MAX_HOSTNAME_LEN: usize = 253;
type ValidationError = (String, ValidationReason);
type ValidationResult = std::result::Result<String, ValidationError>;

/// Logical process name used by channels and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct ProcessName(String);

impl ProcessName {
    /// Creates a validated process name.
    ///
    /// Validation rules:
    /// - non-empty
    /// - max 64 characters
    /// - first character is ASCII letter or digit
    /// - remaining characters are lowercase ASCII letters, digits, `-`, or `_`
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = validate_identifier(value.into())
            .map_err(|(value, reason)| LavaFlowError::InvalidProcessName { value, reason })?;
        Ok(Self(value))
    }

    /// Returns the process name as a borrowed string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the owned inner string.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<'de> Deserialize<'de> for ProcessName {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        ProcessName::new(value).map_err(serde::de::Error::custom)
    }
}

/// Stable identifier for a communication channel.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct ChannelId(String);

impl ChannelId {
    /// Creates a validated channel identifier.
    ///
    /// Validation rules are the same as [`ProcessName::new`].
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = validate_identifier(value.into())
            .map_err(|(value, reason)| LavaFlowError::InvalidChannelId { value, reason })?;
        Ok(Self(value))
    }

    /// Returns the channel identifier as a borrowed string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the owned inner string.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<'de> Deserialize<'de> for ChannelId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        ChannelId::new(value).map_err(serde::de::Error::custom)
    }
}

/// Location metadata used for topology-aware routing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessLocation {
    #[serde(deserialize_with = "deserialize_hostname")]
    hostname: String,
    node_id: Option<u32>,
    device_id: Option<u32>,
}

impl ProcessLocation {
    /// Creates a validated location with hostname only.
    ///
    /// Hostname validation rules:
    /// - non-empty
    /// - max 253 characters
    /// - first character is ASCII letter or digit
    /// - remaining characters are ASCII letters, digits, `-`, `.`, or `_`
    ///
    /// Hostnames are normalized to lowercase for stable scope comparison.
    pub fn new(hostname: impl Into<String>) -> Result<Self> {
        let hostname = validate_hostname(hostname.into())
            .map_err(|(value, reason)| LavaFlowError::InvalidHostname { value, reason })?;
        Ok(Self {
            hostname,
            node_id: None,
            device_id: None,
        })
    }

    /// Creates a validated location with hostname plus optional node/device metadata.
    pub fn with_ids(
        hostname: impl Into<String>,
        node_id: Option<u32>,
        device_id: Option<u32>,
    ) -> Result<Self> {
        let hostname = validate_hostname(hostname.into())
            .map_err(|(value, reason)| LavaFlowError::InvalidHostname { value, reason })?;
        Ok(Self {
            hostname,
            node_id,
            device_id,
        })
    }

    /// Detects the local hostname via OS APIs and returns a location.
    ///
    /// Returns an error if hostname detection fails.
    pub fn from_hostname() -> Result<Self> {
        let hostname = hostname::get().map_err(LavaFlowError::HostnameDetection)?;
        let hostname = hostname.to_string_lossy().trim().to_string();
        Self::new(hostname)
    }

    /// Returns the normalized hostname.
    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    /// Returns the optional scheduler/node index metadata.
    pub fn node_id(&self) -> Option<u32> {
        self.node_id
    }

    /// Returns the optional GPU/device index metadata.
    pub fn device_id(&self) -> Option<u32> {
        self.device_id
    }
}

/// Communication scope derived from locations.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommunicationScope {
    /// Sender and peer are on the same host.
    Local,
    /// Sender and peer are on different hosts, or host identity is ambiguous.
    Remote,
}

impl CommunicationScope {
    /// Computes communication scope from two process locations.
    pub fn from_locations(my_location: &ProcessLocation, peer_location: &ProcessLocation) -> Self {
        if my_location.hostname == peer_location.hostname {
            Self::Local
        } else {
            Self::Remote
        }
    }
}

/// Detects communication scope between two process locations.
pub fn detect_scope(
    my_location: &ProcessLocation,
    peer_location: &ProcessLocation,
) -> CommunicationScope {
    CommunicationScope::from_locations(my_location, peer_location)
}

fn validate_identifier(value: String) -> ValidationResult {
    // Shared normalization/validation path for name-like identifiers to keep
    // behavioral rules consistent across all public wrapper types.
    if value.is_empty() {
        return Err((value, ValidationReason::Empty));
    }
    if value.len() > MAX_IDENTIFIER_LEN {
        return Err((value, ValidationReason::IdentifierTooLong));
    }
    let mut chars = value.chars();
    let first = chars.next().expect("identifier checked non-empty");
    if !first.is_ascii_alphanumeric() {
        return Err((value, ValidationReason::InvalidStartCharacter));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err((value, ValidationReason::InvalidCharacters));
    }
    Ok(value)
}

fn validate_hostname(value: String) -> ValidationResult {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err((value, ValidationReason::Empty));
    }
    if value.len() > MAX_HOSTNAME_LEN {
        return Err((value, ValidationReason::HostnameTooLong));
    }
    let mut chars = value.chars();
    let first = chars.next().expect("hostname checked non-empty");
    if !first.is_ascii_alphanumeric() {
        return Err((value, ValidationReason::InvalidStartCharacter));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
    {
        return Err((value, ValidationReason::InvalidCharacters));
    }
    Ok(value.to_ascii_lowercase())
}

fn deserialize_hostname<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    validate_hostname(value).map_err(|(value, reason)| {
        serde::de::Error::custom(LavaFlowError::InvalidHostname { value, reason })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn process_name_accepts_valid_identifiers() {
        let name = ProcessName::new("gpu_worker_01").expect("valid process name");
        assert_eq!(name.as_str(), "gpu_worker_01");
    }

    #[test]
    fn process_name_into_inner_returns_owned_value() {
        let name = ProcessName::new("gpu_worker_01").expect("valid process name");
        assert_eq!(name.into_inner(), "gpu_worker_01");
    }

    #[test]
    fn process_name_rejects_invalid_identifiers() {
        let err = ProcessName::new("GPU-WORKER").expect_err("expected validation error");
        assert!(matches!(
            err,
            LavaFlowError::InvalidProcessName {
                reason: ValidationReason::InvalidCharacters,
                ..
            }
        ));
    }

    #[test]
    fn process_name_rejects_empty_identifiers() {
        let err = ProcessName::new("").expect_err("expected validation error");
        assert!(matches!(
            err,
            LavaFlowError::InvalidProcessName {
                reason: ValidationReason::Empty,
                ..
            }
        ));
    }

    #[test]
    fn process_name_rejects_too_long_identifiers() {
        let too_long = "a".repeat(MAX_IDENTIFIER_LEN + 1);
        let err = ProcessName::new(too_long).expect_err("expected validation error");
        assert!(matches!(
            err,
            LavaFlowError::InvalidProcessName {
                reason: ValidationReason::IdentifierTooLong,
                ..
            }
        ));
    }

    #[test]
    fn process_name_rejects_invalid_start_identifiers() {
        let err = ProcessName::new("-gpu").expect_err("expected validation error");
        assert!(matches!(
            err,
            LavaFlowError::InvalidProcessName {
                reason: ValidationReason::InvalidStartCharacter,
                ..
            }
        ));
    }

    #[test]
    fn channel_id_accepts_valid_identifiers() {
        let channel_id = ChannelId::new("channel-0").expect("valid channel id");
        assert_eq!(channel_id.as_str(), "channel-0");
    }

    #[test]
    fn channel_id_into_inner_returns_owned_value() {
        let channel_id = ChannelId::new("channel-0").expect("valid channel id");
        assert_eq!(channel_id.into_inner(), "channel-0");
    }

    #[test]
    fn channel_id_rejects_invalid_identifiers() {
        let err = ChannelId::new("-invalid").expect_err("expected validation error");
        assert!(matches!(
            err,
            LavaFlowError::InvalidChannelId {
                reason: ValidationReason::InvalidStartCharacter,
                ..
            }
        ));
    }

    #[test]
    fn process_location_with_ids_preserves_metadata() {
        let location =
            ProcessLocation::with_ids("gpu-node-0", Some(7), Some(3)).expect("valid location");
        assert_eq!(location.hostname(), "gpu-node-0");
        assert_eq!(location.node_id(), Some(7));
        assert_eq!(location.device_id(), Some(3));
    }

    #[test]
    fn process_location_with_ids_rejects_invalid_hostname() {
        let err = ProcessLocation::with_ids("gpu node 0", Some(7), Some(3))
            .expect_err("expected hostname validation error");
        assert!(matches!(
            err,
            LavaFlowError::InvalidHostname {
                reason: ValidationReason::InvalidCharacters,
                ..
            }
        ));
    }

    #[test]
    fn process_location_from_hostname_works() {
        let location = ProcessLocation::from_hostname().expect("hostname lookup should succeed");
        assert!(!location.hostname().is_empty());
    }

    #[test]
    fn scope_detection_is_local_when_hostnames_match() {
        let my_location = ProcessLocation::new("gpu-node-0").expect("valid hostname");
        let peer_location = ProcessLocation::new("gpu-node-0").expect("valid hostname");

        let scope = CommunicationScope::from_locations(&my_location, &peer_location);

        assert_eq!(scope, CommunicationScope::Local);
    }

    #[test]
    fn scope_detection_is_remote_when_hostnames_differ() {
        let my_location = ProcessLocation::new("gpu-node-0").expect("valid hostname");
        let peer_location = ProcessLocation::new("gpu-node-1").expect("valid hostname");

        let scope = CommunicationScope::from_locations(&my_location, &peer_location);

        assert_eq!(scope, CommunicationScope::Remote);
    }

    #[test]
    fn scope_detection_is_case_insensitive_after_normalization() {
        let my_location = ProcessLocation::new("GPU-NODE-0").expect("valid hostname");
        let peer_location = ProcessLocation::new("gpu-node-0").expect("valid hostname");

        let scope = detect_scope(&my_location, &peer_location);

        assert_eq!(scope, CommunicationScope::Local);
    }

    #[test]
    fn process_location_rejects_empty_hostname() {
        let err = ProcessLocation::new("").expect_err("expected hostname validation error");

        assert!(matches!(
            err,
            LavaFlowError::InvalidHostname {
                reason: ValidationReason::Empty,
                ..
            }
        ));
    }

    #[test]
    fn process_location_rejects_invalid_hostname_characters() {
        let err =
            ProcessLocation::new("gpu node 0").expect_err("expected hostname validation error");

        assert!(matches!(
            err,
            LavaFlowError::InvalidHostname {
                reason: ValidationReason::InvalidCharacters,
                ..
            }
        ));
    }

    #[test]
    fn process_location_rejects_invalid_start_hostname() {
        let err =
            ProcessLocation::new("-gpu-node-0").expect_err("expected hostname validation error");

        assert!(matches!(
            err,
            LavaFlowError::InvalidHostname {
                reason: ValidationReason::InvalidStartCharacter,
                ..
            }
        ));
    }

    #[test]
    fn process_location_rejects_too_long_hostname() {
        let too_long = "a".repeat(MAX_HOSTNAME_LEN + 1);
        let err = ProcessLocation::new(too_long).expect_err("expected hostname validation error");

        assert!(matches!(
            err,
            LavaFlowError::InvalidHostname {
                reason: ValidationReason::HostnameTooLong,
                ..
            }
        ));
    }

    #[test]
    fn process_name_deserialize_rejects_invalid_value() {
        let err = serde_json::from_str::<ProcessName>("\"GPU-WORKER\"")
            .expect_err("expected deserialization validation error");
        assert!(err.to_string().contains("invalid process name"));
    }

    #[test]
    fn channel_id_deserialize_rejects_invalid_value() {
        let err = serde_json::from_str::<ChannelId>("\"-invalid\"")
            .expect_err("expected deserialization validation error");
        assert!(err.to_string().contains("invalid channel id"));
    }

    #[test]
    fn process_location_deserialize_rejects_invalid_hostname() {
        let payload = json!({
            "hostname": "gpu node 0",
            "node_id": 1,
            "device_id": 0
        })
        .to_string();
        let err = serde_json::from_str::<ProcessLocation>(&payload)
            .expect_err("expected deserialization validation error");
        assert!(err.to_string().contains("invalid hostname"));
    }

    #[test]
    fn process_location_deserialize_normalizes_hostname() {
        let payload = json!({
            "hostname": "GPU-NODE-0",
            "node_id": 1,
            "device_id": 0
        })
        .to_string();
        let location =
            serde_json::from_str::<ProcessLocation>(&payload).expect("valid deserialization");
        assert_eq!(location.hostname(), "gpu-node-0");
        assert_eq!(location.node_id(), Some(1));
        assert_eq!(location.device_id(), Some(0));
    }
}
