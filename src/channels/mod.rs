//! Layer-2 channel envelope types and local transport scaffolding.

use crate::error::{LavaFlowError, Result};
use crate::memory::{cpu, gpu};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::convert::TryFrom;

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod local;

/// Metadata encoding configuration for channel envelopes.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum MetadataEncoding {
    /// JSON metadata encoding.
    #[default]
    Json = 0,
    /// CBOR metadata encoding.
    ///
    /// This variant is planned but not implemented in the current step-1 local transport.
    Cbor = 1,
}

impl TryFrom<u8> for MetadataEncoding {
    type Error = LavaFlowError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Json),
            1 => Ok(Self::Cbor),
            _ => Err(LavaFlowError::ChannelTransportOperation {
                operation: "decode_connection_header",
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "unknown metadata encoding",
                ),
            }),
        }
    }
}

/// User-defined typed metadata contract for channel payloads.
pub trait ChannelMetadata: Serialize + DeserializeOwned {
    /// Returns the number of payload bytes that are valid for this message.
    fn used_size(&self) -> usize;
}

/// Dynamic metadata value used by schema-less receive paths.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MetaValue {
    /// Boolean value.
    Bool(bool),
    /// Signed integer value.
    I64(i64),
    /// Unsigned integer value.
    U64(u64),
    /// Floating-point value.
    F64(f64),
    /// UTF-8 string value.
    String(String),
    /// Opaque byte string.
    Bytes(Vec<u8>),
    /// Homogeneous or heterogeneous list.
    List(Vec<MetaValue>),
    /// Nested map value.
    Map(BTreeMap<String, MetaValue>),
}

/// Dynamic metadata envelope for schema-less receive operations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageMeta {
    /// Number of payload bytes that are valid for this message.
    pub used_size: usize,
    /// Additional metadata fields.
    pub values: BTreeMap<String, MetaValue>,
}

impl ChannelMetadata for MessageMeta {
    fn used_size(&self) -> usize {
        self.used_size
    }
}

/// Observable receive behavior for a receiver endpoint.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReceiveRepresentation {
    /// Receiver returns an externally shared buffer reference.
    ExternalShare,
    /// Receiver materializes into an owned buffer.
    Materialized,
}

/// Payload exchanged by channels.
///
/// `Frame` is the high-level payload abstraction used by the public channel API. It distinguishes
/// buffer backend kind (`CPU` or `GPU`) while keeping transport representation details internal to
/// the channel runtime. Whether delivery used external sharing or local materialization is exposed
/// through receiver introspection such as [`ReceiveRepresentation`].
#[derive(Debug)]
pub enum Frame {
    /// CPU-backed payload.
    Cpu(cpu::MemoryBuffer),
    /// GPU-backed payload.
    Gpu(gpu::MemoryBuffer),
}

impl Frame {
    /// Returns the payload size in bytes.
    pub fn size(&self) -> usize {
        match self {
            Self::Cpu(buffer) => buffer.size(),
            Self::Gpu(buffer) => buffer.size(),
        }
    }

    /// Returns the CPU buffer when this payload is CPU-backed.
    pub fn into_cpu(self) -> Option<cpu::MemoryBuffer> {
        match self {
            Self::Cpu(buffer) => Some(buffer),
            Self::Gpu(_) => None,
        }
    }

    /// Returns the GPU buffer when this payload is GPU-backed.
    pub fn into_gpu(self) -> Option<gpu::MemoryBuffer> {
        match self {
            Self::Cpu(_) => None,
            Self::Gpu(buffer) => Some(buffer),
        }
    }
}

impl From<cpu::MemoryBuffer> for Frame {
    fn from(buffer: cpu::MemoryBuffer) -> Self {
        Self::Cpu(buffer)
    }
}

impl From<gpu::MemoryBuffer> for Frame {
    fn from(buffer: gpu::MemoryBuffer) -> Self {
        Self::Gpu(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_encoding_rejects_unknown_wire_value() {
        let err = MetadataEncoding::try_from(99).expect_err("unknown encoding must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "decode_connection_header",
                ..
            }
        ));
    }

    #[test]
    fn message_meta_used_size_returns_stored_value() {
        let meta = MessageMeta {
            used_size: 17,
            values: BTreeMap::new(),
        };
        assert_eq!(meta.used_size(), 17);
    }

    #[test]
    fn frame_cpu_helpers_cover_size_and_variant_access() {
        let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(64)
            .expect("allocate cpu buffer");
        let frame = Frame::from(buffer);
        assert_eq!(frame.size(), 64);
        assert!(frame.into_cpu().is_some());

        let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(64)
            .expect("allocate cpu buffer");
        let frame = Frame::from(buffer);
        assert!(frame.into_gpu().is_none());
    }

    #[test]
    fn frame_gpu_helpers_cover_size_and_variant_access() {
        let allocator = match gpu::Allocator::new() {
            Ok(allocator) => allocator,
            Err(_) => return,
        };
        let buffer = allocator.allocate(64).expect("allocate gpu buffer");
        let frame = Frame::from(buffer);
        assert_eq!(frame.size(), 64);
        assert!(frame.into_gpu().is_some());

        let buffer = allocator.allocate(64).expect("allocate gpu buffer");
        let frame = Frame::from(buffer);
        assert!(frame.into_cpu().is_none());
    }
}
