//! Layer-2 channel envelope types and local transport scaffolding.

use crate::error::{LavaFlowError, Result};
use crate::memory::{cpu, gpu};
use crate::types::{ChannelId, CommunicationScope, ProcessLocation, detect_scope};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) mod local;

/// Entry point for constructing directional channel endpoints.
#[derive(Debug)]
pub struct Builder;

impl Builder {
    /// Starts a sender-endpoint builder for the given channel and peer topology.
    pub fn sender(
        channel_id: ChannelId,
        my_location: ProcessLocation,
        peer_location: ProcessLocation,
    ) -> SenderBuilder {
        SenderBuilder {
            options: BuilderOptions::new(channel_id, detect_scope(&my_location, &peer_location)),
            metadata_encoding: MetadataEncoding::default(),
        }
    }

    /// Starts a receiver-endpoint builder for the given channel and peer topology.
    pub fn receiver(
        channel_id: ChannelId,
        my_location: ProcessLocation,
        peer_location: ProcessLocation,
    ) -> ReceiverBuilder {
        ReceiverBuilder {
            options: BuilderOptions::new(channel_id, detect_scope(&my_location, &peer_location)),
        }
    }

    /// Starts a local sender-endpoint builder using the current hostname for both peers.
    pub fn local_sender(channel_id: ChannelId) -> Result<SenderBuilder> {
        let location = ProcessLocation::from_hostname()?;
        Ok(Self::sender(channel_id, location.clone(), location))
    }

    /// Starts a local receiver-endpoint builder using the current hostname for both peers.
    pub fn local_receiver(channel_id: ChannelId) -> Result<ReceiverBuilder> {
        let location = ProcessLocation::from_hostname()?;
        Ok(Self::receiver(channel_id, location.clone(), location))
    }
}

/// Cooperative cancellation handle for blocking channel endpoint construction.
///
/// Clones share the same cancellation state. Pass one clone to a blocking builder method and keep
/// another clone in the supervisor thread so it can call [`BuildCancel::cancel`].
#[derive(Clone, Debug, Default)]
pub struct BuildCancel {
    cancelled: Arc<AtomicBool>,
}

impl BuildCancel {
    /// Creates a new non-cancelled build cancellation handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation of any builder currently observing this handle.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Returns whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Debug)]
struct BuilderOptions {
    channel_id: ChannelId,
    scope: CommunicationScope,
    local_max_payload_size: usize,
    local_max_metadata_size: usize,
    local_access: local::Access,
}

impl BuilderOptions {
    fn new(channel_id: ChannelId, scope: CommunicationScope) -> Self {
        Self {
            channel_id,
            scope,
            local_max_payload_size: 0,
            local_max_metadata_size: 0,
            local_access: local::Access::default(),
        }
    }

    fn with_local_max_payload_size(mut self, max_payload_size: usize) -> Self {
        self.local_max_payload_size = max_payload_size;
        self
    }

    fn with_local_max_metadata_size(mut self, max_metadata_size: usize) -> Self {
        self.local_max_metadata_size = max_metadata_size;
        self
    }

    fn with_local_access(mut self, access: local::Access) -> Self {
        self.local_access = access;
        self
    }

    fn local_limits(&self) -> local::ProtocolLimits {
        local::ProtocolLimits::with_max_sizes(
            self.local_max_payload_size,
            self.local_max_metadata_size,
        )
    }

    fn local_access(&self) -> local::Access {
        self.local_access
    }
}

/// Builder for sender endpoints.
#[derive(Debug)]
pub struct SenderBuilder {
    options: BuilderOptions,
    metadata_encoding: MetadataEncoding,
}

impl SenderBuilder {
    const ACCEPT_POLL_DELAY: Duration = Duration::from_millis(10);

    /// Sets the metadata encoding used for outbound messages on this endpoint.
    pub fn with_metadata_encoding(mut self, encoding: MetadataEncoding) -> Self {
        self.metadata_encoding = encoding;
        self
    }

    /// Overrides the local maximum payload size in bytes.
    ///
    /// A value of `0` keeps the built-in default.
    pub fn with_local_max_payload_size(mut self, max_payload_size: usize) -> Self {
        self.options = self.options.with_local_max_payload_size(max_payload_size);
        self
    }

    /// Overrides the local maximum metadata size in bytes.
    ///
    /// A value of `0` keeps the built-in default.
    pub fn with_local_max_metadata_size(mut self, max_metadata_size: usize) -> Self {
        self.options = self.options.with_local_max_metadata_size(max_metadata_size);
        self
    }

    /// Restricts local IPC to the current Windows logon session or Unix user.
    ///
    /// This is the default local access policy.
    pub fn with_current_session_local_access(mut self) -> Self {
        self.options = self
            .options
            .with_local_access(local::Access::CurrentSession);
        self
    }

    /// Allows authenticated local OS users to connect to the local IPC endpoint.
    ///
    /// Use this only when peers are expected to run as different local users.
    pub fn with_authenticated_users_local_access(mut self) -> Self {
        self.options = self
            .options
            .with_local_access(local::Access::AuthenticatedUsers);
        self
    }

    /// Builds a sender endpoint.
    ///
    /// The current implementation supports only local scope. Remote transports remain deferred to
    /// a later phase. For local scope this call blocks until the receiver peer connects.
    pub fn build(self) -> Result<Sender> {
        self.build_internal(None, None)
    }

    /// Builds a sender endpoint, waiting at most `timeout` for the receiver peer to connect.
    pub fn build_with_timeout(self, timeout: Duration) -> Result<Sender> {
        self.build_internal(Some(timeout), None)
    }

    /// Builds a sender endpoint until the receiver peer connects or `cancel` is cancelled.
    pub fn build_or_cancelled(self, cancel: BuildCancel) -> Result<Sender> {
        self.build_internal(None, Some(&cancel))
    }

    /// Builds a sender endpoint until the receiver peer connects, `timeout` elapses, or `cancel`
    /// is cancelled.
    pub fn build_with_timeout_or_cancel(
        self,
        timeout: Duration,
        cancel: BuildCancel,
    ) -> Result<Sender> {
        self.build_internal(Some(timeout), Some(&cancel))
    }

    fn build_internal(
        self,
        timeout: Option<Duration>,
        cancel: Option<&BuildCancel>,
    ) -> Result<Sender> {
        match self.options.scope {
            CommunicationScope::Local => {
                let limits = self.options.local_limits();
                let listener = local::listen(
                    &self.options.channel_id,
                    self.metadata_encoding,
                    limits,
                    self.options.local_access(),
                )?;
                let inner = match (timeout, cancel) {
                    (None, None) => listener.accept()?,
                    (timeout, cancel) => listener.accept_with_control(
                        timeout,
                        cancel,
                        Self::ACCEPT_POLL_DELAY,
                        "sender",
                    )?,
                };
                Ok(Sender {
                    inner: SenderImpl::Local(inner),
                })
            }
            scope => Err(LavaFlowError::UnsupportedCommunicationScope { scope }),
        }
    }
}

/// Builder for receiver endpoints.
#[derive(Debug)]
pub struct ReceiverBuilder {
    options: BuilderOptions,
}

impl ReceiverBuilder {
    const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(10);

    /// Overrides the local maximum payload size in bytes.
    ///
    /// A value of `0` keeps the built-in default.
    pub fn with_local_max_payload_size(mut self, max_payload_size: usize) -> Self {
        self.options = self.options.with_local_max_payload_size(max_payload_size);
        self
    }

    /// Overrides the local maximum metadata size in bytes.
    ///
    /// A value of `0` keeps the built-in default.
    pub fn with_local_max_metadata_size(mut self, max_metadata_size: usize) -> Self {
        self.options = self.options.with_local_max_metadata_size(max_metadata_size);
        self
    }

    /// Restricts local IPC to the current Windows logon session or Unix user.
    ///
    /// This is the default local access policy.
    pub fn with_current_session_local_access(mut self) -> Self {
        self.options = self
            .options
            .with_local_access(local::Access::CurrentSession);
        self
    }

    /// Allows authenticated local OS users to connect to the local IPC endpoint.
    ///
    /// Use this only when peers are expected to run as different local users.
    pub fn with_authenticated_users_local_access(mut self) -> Self {
        self.options = self
            .options
            .with_local_access(local::Access::AuthenticatedUsers);
        self
    }

    /// Builds a receiver endpoint.
    pub fn build(self) -> Result<Receiver> {
        self.build_internal(None, None)
    }

    /// Builds a receiver endpoint, retrying transient local connect failures until `timeout`
    /// elapses.
    ///
    /// This is useful when local peer startup order is non-deterministic, such as when the sender
    /// process is launched by an external supervisor and may not have completed its listener bind
    /// yet.
    pub fn build_with_timeout(self, timeout: Duration) -> Result<Receiver> {
        self.build_internal(Some(timeout), None)
    }

    /// Builds a receiver endpoint until the sender peer is reachable or `cancel` is cancelled.
    pub fn build_or_cancelled(self, cancel: BuildCancel) -> Result<Receiver> {
        self.build_internal(None, Some(&cancel))
    }

    /// Builds a receiver endpoint until the sender peer is reachable, `timeout` elapses, or
    /// `cancel` is cancelled.
    pub fn build_with_timeout_or_cancel(
        self,
        timeout: Duration,
        cancel: BuildCancel,
    ) -> Result<Receiver> {
        self.build_internal(Some(timeout), Some(&cancel))
    }

    fn build_internal(
        self,
        timeout: Option<Duration>,
        cancel: Option<&BuildCancel>,
    ) -> Result<Receiver> {
        match self.options.scope {
            CommunicationScope::Local => {
                let limits = self.options.local_limits();
                let inner = match (timeout, cancel) {
                    (None, None) => local::connect(
                        &self.options.channel_id,
                        limits,
                        self.options.local_access(),
                    )?,
                    (timeout, cancel) => {
                        self.connect_local_with_control(limits, timeout, cancel)?
                    }
                };
                Ok(Receiver {
                    inner: ReceiverImpl::Local(inner),
                })
            }
            scope => Err(LavaFlowError::UnsupportedCommunicationScope { scope }),
        }
    }

    fn connect_local_with_control(
        &self,
        limits: local::ProtocolLimits,
        timeout: Option<Duration>,
        cancel: Option<&BuildCancel>,
    ) -> Result<local::Receiver> {
        let start = Instant::now();

        loop {
            if cancel.is_some_and(BuildCancel::is_cancelled) {
                return Err(LavaFlowError::ChannelBuildCancelled {
                    endpoint: "receiver",
                });
            }

            match local::connect(
                &self.options.channel_id,
                limits,
                self.options.local_access(),
            ) {
                Ok(receiver) => return Ok(receiver),
                Err(err) if local::is_retryable_connect_error(&err) => {
                    if let Some(timeout) = timeout
                        && start.elapsed() >= timeout
                    {
                        return Err(Self::connect_timeout_error(timeout, &err));
                    }
                    thread::sleep(Self::CONNECT_RETRY_DELAY);
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn connect_timeout_error(timeout: Duration, last_error: &LavaFlowError) -> LavaFlowError {
        let details =
            format!("receiver connect timed out after {timeout:?}; last error: {last_error}");
        LavaFlowError::ChannelTransportOperation {
            operation: "connect",
            source: io::Error::new(io::ErrorKind::TimedOut, details),
        }
    }
}

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
///
/// Any serde-serializable and deserializable type can be used as metadata. Buffer sizing is carried
/// by the channel protocol; applications may include their own valid-byte count or other payload
/// interpretation fields when needed.
pub trait Metadata: Serialize + DeserializeOwned {}

impl<T> Metadata for T where T: Serialize + DeserializeOwned {}

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
    /// Dynamic metadata fields.
    pub values: BTreeMap<String, MetaValue>,
}

/// Observable receive behavior for a receiver endpoint.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ReceiveRepresentation {
    /// Receiver returns an externally shared buffer reference.
    ExternalShare,
    /// Receiver materializes into an owned buffer.
    Materialized,
}

/// Directional sender endpoint for channel payloads and metadata.
#[derive(Debug)]
pub struct Sender {
    inner: SenderImpl,
}

#[derive(Debug)]
enum SenderImpl {
    Local(local::Sender),
}

impl Sender {
    /// Sends a payload frame with typed metadata.
    pub fn send<M, F>(&mut self, frame: F, metadata: &M) -> Result<()>
    where
        M: Metadata,
        F: Into<Frame>,
    {
        match &mut self.inner {
            SenderImpl::Local(sender) => sender.send(frame, metadata),
        }
    }

    /// Sends a payload frame with dynamic metadata.
    pub fn send_map<F>(&mut self, frame: F, metadata: MessageMeta) -> Result<()>
    where
        F: Into<Frame>,
    {
        match &mut self.inner {
            SenderImpl::Local(sender) => sender.send_map(frame, metadata),
        }
    }

    /// Returns the communication scope selected for this endpoint.
    pub fn scope(&self) -> CommunicationScope {
        match &self.inner {
            SenderImpl::Local(_) => CommunicationScope::Local,
        }
    }
}

/// Directional receiver endpoint for channel payloads and metadata.
#[derive(Debug)]
pub struct Receiver {
    inner: ReceiverImpl,
}

#[derive(Debug)]
enum ReceiverImpl {
    Local(local::Receiver),
}

impl Receiver {
    /// Receives a payload frame with typed metadata.
    pub fn recv<M: Metadata>(&mut self) -> Result<(Frame, M)> {
        match &mut self.inner {
            ReceiverImpl::Local(receiver) => receiver.recv(),
        }
    }

    /// Receives a payload frame with dynamic metadata.
    pub fn recv_map(&mut self) -> Result<(Frame, MessageMeta)> {
        match &mut self.inner {
            ReceiverImpl::Local(receiver) => receiver.recv_map(),
        }
    }

    /// Returns the communication scope selected for this endpoint.
    pub fn scope(&self) -> CommunicationScope {
        match &self.inner {
            ReceiverImpl::Local(_) => CommunicationScope::Local,
        }
    }

    /// Returns how this receiver represents received payload memory locally.
    pub fn receive_representation(&self) -> ReceiveRepresentation {
        match &self.inner {
            ReceiverImpl::Local(_) => ReceiveRepresentation::ExternalShare,
        }
    }
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
    use crate::types::ChannelId;
    use crate::types::ProcessLocation;
    use serde::{Deserialize, Serialize};
    use std::sync::atomic::{AtomicU64, Ordering};

    static CHANNEL_TEST_COUNTER: AtomicU64 = AtomicU64::new(1);
    const TEST_RECEIVER_BUILD_TIMEOUT: Duration = Duration::from_secs(5);

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct PublicTestMeta {
        width: u32,
        height: u32,
    }

    fn build_local_receiver_with_limits(
        channel_id: ChannelId,
        max_payload: usize,
        max_metadata: usize,
    ) -> Receiver {
        let start = Instant::now();

        loop {
            match Builder::receiver(
                channel_id.clone(),
                ProcessLocation::new("node-0").expect("receiver location"),
                ProcessLocation::new("node-0").expect("sender location"),
            )
            .with_current_session_local_access()
            .with_local_max_payload_size(max_payload)
            .with_local_max_metadata_size(max_metadata)
            .build()
            {
                Ok(receiver) => return receiver,
                Err(err) if local::is_retryable_connect_error(&err) => {
                    if start.elapsed() >= TEST_RECEIVER_BUILD_TIMEOUT {
                        panic!(
                            "build receiver after timeout {:?}: {:?}",
                            TEST_RECEIVER_BUILD_TIMEOUT, err
                        );
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("build receiver: {err:?}"),
            }
        }
    }

    fn build_local_receiver_with_timeout(channel_id: ChannelId, max_payload: usize) -> Receiver {
        build_local_receiver_with_limits(channel_id, max_payload, 0)
    }

    fn build_local_receiver_with_hostname_timeout(channel_id: ChannelId) -> Receiver {
        let start = Instant::now();

        loop {
            match Builder::local_receiver(channel_id.clone())
                .expect("receiver builder")
                .build()
            {
                Ok(receiver) => return receiver,
                Err(err) if local::is_retryable_connect_error(&err) => {
                    if start.elapsed() >= TEST_RECEIVER_BUILD_TIMEOUT {
                        panic!(
                            "build receiver from hostname after timeout {:?}: {:?}",
                            TEST_RECEIVER_BUILD_TIMEOUT, err
                        );
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("build receiver from hostname: {err:?}"),
            }
        }
    }

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

    #[test]
    fn local_builder_sender_and_receiver_round_trip_typed_metadata() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("round-trip-typed", id);
        let channel_id =
            ChannelId::new(format!("builder-local-round-trip-{id}")).expect("channel id");
        let receiver_channel_id = channel_id.clone();
        let receiver_thread = thread::spawn(move || {
            let mut receiver = build_local_receiver_with_timeout(receiver_channel_id, 0);
            let (frame, received) = receiver.recv_map().expect("receive message");
            assert!(matches!(frame, Frame::Cpu(_)));
            (
                received,
                receiver.scope(),
                receiver.receive_representation(),
            )
        });

        let mut sender = Builder::sender(
            channel_id,
            ProcessLocation::new("node-0").expect("sender location"),
            ProcessLocation::new("node-0").expect("receiver location"),
        )
        .build()
        .expect("build sender");
        let metadata = MessageMeta {
            values: BTreeMap::from([("count".into(), MetaValue::U64(7))]),
        };
        let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(64)
            .expect("allocate payload");
        sender
            .send_map(buffer, metadata.clone())
            .expect("send message");
        let (received, receiver_scope, representation) = receiver_thread
            .join()
            .expect("receiver thread must not panic");

        assert_eq!(received, metadata);
        assert_eq!(sender.scope(), CommunicationScope::Local);
        assert_eq!(receiver_scope, CommunicationScope::Local);
        assert_eq!(representation, ReceiveRepresentation::ExternalShare);
    }

    #[test]
    fn local_builder_public_typed_send_and_recv_round_trip() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("public-typed", id);
        let channel_id =
            ChannelId::new(format!("builder-local-typed-round-trip-{id}")).expect("channel id");
        let receiver_channel_id = channel_id.clone();
        let expected = PublicTestMeta {
            width: 800,
            height: 600,
        };
        let receiver_thread = thread::spawn(move || {
            let mut receiver = build_local_receiver_with_timeout(receiver_channel_id, 0);
            let (frame, metadata) = receiver
                .recv::<PublicTestMeta>()
                .expect("receive typed message");
            assert!(matches!(frame, Frame::Cpu(_)));
            (metadata, receiver.scope())
        });

        let mut sender = Builder::sender(
            channel_id,
            ProcessLocation::new("node-0").expect("sender location"),
            ProcessLocation::new("node-0").expect("receiver location"),
        )
        .with_metadata_encoding(MetadataEncoding::Json)
        .build()
        .expect("build sender");
        let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(64)
            .expect("allocate payload");
        sender.send(buffer, &expected).expect("send typed message");

        let (received, receiver_scope) = receiver_thread
            .join()
            .expect("receiver thread must not panic");
        assert_eq!(received, expected);
        assert_eq!(sender.scope(), CommunicationScope::Local);
        assert_eq!(receiver_scope, CommunicationScope::Local);
    }

    #[test]
    fn local_builder_sender_limit_override_rejects_oversized_payload() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("limits", id);
        let channel_id = ChannelId::new(format!("builder-local-limits-{id}")).expect("channel id");
        let receiver_channel_id = channel_id.clone();
        let receiver_thread =
            thread::spawn(move || build_local_receiver_with_timeout(receiver_channel_id, 8));

        let mut sender = Builder::sender(
            channel_id,
            ProcessLocation::new("node-0").expect("sender location"),
            ProcessLocation::new("node-0").expect("receiver location"),
        )
        .with_local_max_payload_size(8)
        .build()
        .expect("build sender");
        let _receiver = receiver_thread
            .join()
            .expect("receiver thread must not panic");
        let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(64)
            .expect("allocate payload");
        let metadata = MessageMeta {
            values: BTreeMap::new(),
        };

        let err = sender
            .send_map(buffer, metadata)
            .expect_err("oversized payload must be rejected");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "write_message_envelope",
                ..
            }
        ));
    }

    #[test]
    fn local_builder_metadata_limit_override_rejects_oversized_metadata() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("metadata-limits", id);
        let channel_id =
            ChannelId::new(format!("builder-local-metadata-limits-{id}")).expect("channel id");
        let receiver_channel_id = channel_id.clone();
        let receiver_thread =
            thread::spawn(move || build_local_receiver_with_limits(receiver_channel_id, 0, 8));

        let mut sender = Builder::sender(
            channel_id,
            ProcessLocation::new("node-0").expect("sender location"),
            ProcessLocation::new("node-0").expect("receiver location"),
        )
        .with_local_max_metadata_size(8)
        .build()
        .expect("build sender");
        let _receiver = receiver_thread
            .join()
            .expect("receiver thread must not panic");
        let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(64)
            .expect("allocate payload");
        let metadata = MessageMeta {
            values: BTreeMap::from([("blob".into(), MetaValue::Bytes(vec![1_u8; 32]))]),
        };

        let err = sender
            .send_map(buffer, metadata)
            .expect_err("oversized metadata must be rejected");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "write_message_envelope",
                ..
            }
        ));
    }

    #[test]
    fn local_sender_and_receiver_builders_use_hostname_defaults() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("hostname-defaults", id);
        let channel_id =
            ChannelId::new(format!("builder-local-hostname-defaults-{id}")).expect("channel id");
        let receiver_channel_id = channel_id.clone();
        let receiver_thread = thread::spawn(move || {
            let mut receiver = build_local_receiver_with_hostname_timeout(receiver_channel_id);
            let (frame, metadata) = receiver.recv_map().expect("receive message");
            assert!(matches!(frame, Frame::Cpu(_)));
            metadata
        });

        let mut sender = Builder::local_sender(channel_id)
            .expect("sender builder")
            .build()
            .expect("build sender");
        let metadata = MessageMeta {
            values: BTreeMap::from([("flag".into(), MetaValue::Bool(true))]),
        };
        let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(64)
            .expect("allocate payload");
        sender
            .send_map(buffer, metadata.clone())
            .expect("send message");

        let received = receiver_thread
            .join()
            .expect("receiver thread must not panic");
        assert_eq!(received, metadata);
        assert_eq!(sender.scope(), CommunicationScope::Local);
    }

    #[test]
    fn local_builder_authenticated_users_access_round_trips_dynamic_metadata() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let channel_id =
            ChannelId::new(format!("builder-authenticated-users-{id}")).expect("channel id");
        let receiver_channel_id = channel_id.clone();
        let receiver_thread = thread::spawn(move || {
            let mut receiver = Builder::receiver(
                receiver_channel_id,
                ProcessLocation::new("node-0").expect("receiver location"),
                ProcessLocation::new("node-0").expect("sender location"),
            )
            .with_authenticated_users_local_access()
            .build_with_timeout(Duration::from_secs(2))
            .expect("build authenticated-users receiver");
            let (frame, metadata) = receiver.recv_map().expect("receive message");
            assert!(matches!(frame, Frame::Cpu(_)));
            metadata
        });

        let mut sender = Builder::sender(
            channel_id,
            ProcessLocation::new("node-0").expect("sender location"),
            ProcessLocation::new("node-0").expect("receiver location"),
        )
        .with_authenticated_users_local_access()
        .build_with_timeout(Duration::from_secs(2))
        .expect("build authenticated-users sender");
        let metadata = MessageMeta {
            values: BTreeMap::from([("access".into(), MetaValue::String("auth".into()))]),
        };
        let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(64)
            .expect("allocate payload");
        sender
            .send_map(buffer, metadata.clone())
            .expect("send message");

        let received = receiver_thread
            .join()
            .expect("receiver thread must not panic");
        assert_eq!(received, metadata);
    }

    #[test]
    fn local_builder_mixed_access_policies_do_not_rendezvous() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("mixed-access", id);
        let channel_id = ChannelId::new(format!("builder-mixed-access-{id}")).expect("channel id");
        let sender_channel_id = channel_id.clone();
        let sender_thread = thread::spawn(move || {
            Builder::sender(
                sender_channel_id,
                ProcessLocation::new("node-0").expect("sender location"),
                ProcessLocation::new("node-0").expect("receiver location"),
            )
            .with_current_session_local_access()
            .build_with_timeout(Duration::from_millis(100))
            .expect_err("sender must not accept receiver using a different access policy")
        });

        let receiver_err = Builder::receiver(
            channel_id,
            ProcessLocation::new("node-0").expect("receiver location"),
            ProcessLocation::new("node-0").expect("sender location"),
        )
        .with_authenticated_users_local_access()
        .build_with_timeout(Duration::from_millis(50))
        .expect_err("receiver must not connect to sender using a different access policy");

        assert!(matches!(
            receiver_err,
            LavaFlowError::ChannelTransportOperation { operation: "connect", ref source }
                if source.kind() == io::ErrorKind::TimedOut
        ));

        let sender_err = sender_thread.join().expect("sender thread must not panic");
        assert!(matches!(
            sender_err,
            LavaFlowError::ChannelTransportOperation { operation: "accept", ref source }
                if source.kind() == io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn receiver_builder_build_with_timeout_reports_connect_timeout() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("connect-timeout", id);
        let channel_id =
            ChannelId::new(format!("builder-receiver-timeout-{id}")).expect("channel id");
        let err = Builder::receiver(
            channel_id,
            ProcessLocation::new("node-0").expect("receiver location"),
            ProcessLocation::new("node-0").expect("sender location"),
        )
        .build_with_timeout(Duration::from_millis(50))
        .expect_err("receiver connect should time out");

        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation { operation: "connect", ref source }
                if source.kind() == io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn sender_builder_build_with_timeout_reports_accept_timeout() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("sender-accept-timeout", id);
        let channel_id =
            ChannelId::new(format!("builder-sender-timeout-{id}")).expect("channel id");
        let err = Builder::sender(
            channel_id,
            ProcessLocation::new("node-0").expect("sender location"),
            ProcessLocation::new("node-0").expect("receiver location"),
        )
        .build_with_timeout(Duration::from_millis(50))
        .expect_err("sender accept should time out");

        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation { operation: "accept", ref source }
                if source.kind() == io::ErrorKind::TimedOut
        ));
    }

    #[test]
    fn sender_builder_build_or_cancelled_reports_cancellation() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("sender-cancel", id);
        let channel_id = ChannelId::new(format!("builder-sender-cancel-{id}")).expect("channel id");
        let cancel = BuildCancel::new();
        let build_cancel = cancel.clone();
        let builder = Builder::sender(
            channel_id,
            ProcessLocation::new("node-0").expect("sender location"),
            ProcessLocation::new("node-0").expect("receiver location"),
        );
        let build_thread =
            thread::spawn(move || builder.build_or_cancelled(build_cancel).map(|_| ()));

        thread::sleep(Duration::from_millis(50));
        cancel.cancel();

        let err = build_thread
            .join()
            .expect("sender build thread must not panic")
            .expect_err("sender build should be cancelled");
        assert!(matches!(
            err,
            LavaFlowError::ChannelBuildCancelled { endpoint: "sender" }
        ));
    }

    #[test]
    fn receiver_builder_build_or_cancelled_reports_cancellation() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("receiver-cancel", id);
        let channel_id =
            ChannelId::new(format!("builder-receiver-cancel-{id}")).expect("channel id");
        let cancel = BuildCancel::new();
        let build_cancel = cancel.clone();
        let builder = Builder::receiver(
            channel_id,
            ProcessLocation::new("node-0").expect("receiver location"),
            ProcessLocation::new("node-0").expect("sender location"),
        );
        let build_thread =
            thread::spawn(move || builder.build_or_cancelled(build_cancel).map(|_| ()));

        thread::sleep(Duration::from_millis(50));
        cancel.cancel();

        let err = build_thread
            .join()
            .expect("receiver build thread must not panic")
            .expect_err("receiver build should be cancelled");
        assert!(matches!(
            err,
            LavaFlowError::ChannelBuildCancelled {
                endpoint: "receiver"
            }
        ));
    }

    #[test]
    fn receiver_builder_build_with_timeout_allows_delayed_sender_startup() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("delayed-startup", id);
        let channel_id =
            ChannelId::new(format!("builder-receiver-timeout-startup-{id}")).expect("channel id");
        let receiver_channel_id = channel_id.clone();
        let receiver_thread = thread::spawn(move || {
            let mut receiver = Builder::receiver(
                receiver_channel_id,
                ProcessLocation::new("node-0").expect("receiver location"),
                ProcessLocation::new("node-0").expect("sender location"),
            )
            .build_with_timeout(Duration::from_secs(2))
            .expect("build receiver with timeout");
            let (frame, metadata) = receiver.recv_map().expect("receive message");
            assert!(matches!(frame, Frame::Cpu(_)));
            metadata
        });

        thread::sleep(Duration::from_millis(50));

        let mut sender = Builder::sender(
            channel_id,
            ProcessLocation::new("node-0").expect("sender location"),
            ProcessLocation::new("node-0").expect("receiver location"),
        )
        .build()
        .expect("build sender");
        let metadata = MessageMeta {
            values: BTreeMap::from([("delayed".into(), MetaValue::Bool(true))]),
        };
        let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(64)
            .expect("allocate payload");
        sender
            .send_map(buffer, metadata.clone())
            .expect("send message");

        let received = receiver_thread
            .join()
            .expect("receiver thread must not panic");
        assert_eq!(received, metadata);
    }

    #[test]
    fn receiver_builder_build_with_timeout_or_cancel_allows_delayed_sender_startup() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("rx-combo-ok", id);
        let channel_id = ChannelId::new(format!("rx-combo-{id}")).expect("channel id");
        let receiver_channel_id = channel_id.clone();
        let receiver_thread = thread::spawn(move || {
            let mut receiver = Builder::receiver(
                receiver_channel_id,
                ProcessLocation::new("node-0").expect("receiver location"),
                ProcessLocation::new("node-0").expect("sender location"),
            )
            .build_with_timeout_or_cancel(Duration::from_secs(2), BuildCancel::new())
            .expect("build receiver with timeout or cancel");
            let (_frame, metadata) = receiver.recv_map().expect("receive message");
            metadata
        });

        thread::sleep(Duration::from_millis(50));

        let mut sender = Builder::sender(
            channel_id,
            ProcessLocation::new("node-0").expect("sender location"),
            ProcessLocation::new("node-0").expect("receiver location"),
        )
        .build()
        .expect("build sender");
        let metadata = MessageMeta {
            values: BTreeMap::from([("receiver-combined".into(), MetaValue::Bool(true))]),
        };
        let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
            .allocate(64)
            .expect("allocate payload");
        sender
            .send_map(buffer, metadata.clone())
            .expect("send message");

        let received = receiver_thread
            .join()
            .expect("receiver thread must not panic");
        assert_eq!(received, metadata);
    }

    #[test]
    fn sender_builder_build_with_timeout_or_cancel_allows_delayed_receiver_startup() {
        let id = CHANNEL_TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let _runtime_guard = local::stable_test_runtime_dir_guard("tx-combo-ok", id);
        let channel_id = ChannelId::new(format!("tx-combo-{id}")).expect("channel id");
        let sender_channel_id = channel_id.clone();
        let expected = MessageMeta {
            values: BTreeMap::from([("sender-combined".into(), MetaValue::Bool(true))]),
        };
        let sender_metadata = expected.clone();
        let sender_thread = thread::spawn(move || {
            let mut sender = Builder::sender(
                sender_channel_id,
                ProcessLocation::new("node-0").expect("sender location"),
                ProcessLocation::new("node-0").expect("receiver location"),
            )
            .build_with_timeout_or_cancel(Duration::from_secs(2), BuildCancel::new())
            .expect("build sender with timeout or cancel");
            let buffer = cpu::Allocator::with_max_allocation_size(usize::MAX)
                .allocate(64)
                .expect("allocate payload");
            sender
                .send_map(buffer, sender_metadata)
                .expect("send message");
        });

        thread::sleep(Duration::from_millis(50));

        let mut receiver = Builder::receiver(
            channel_id,
            ProcessLocation::new("node-0").expect("receiver location"),
            ProcessLocation::new("node-0").expect("sender location"),
        )
        .build_with_timeout(Duration::from_secs(2))
        .expect("build receiver");
        let (_frame, received) = receiver.recv_map().expect("receive message");

        sender_thread.join().expect("sender thread must not panic");
        assert_eq!(received, expected);
    }

    #[test]
    fn sender_builder_rejects_remote_scope() {
        let err = Builder::sender(
            ChannelId::new("remote-sender").expect("channel id"),
            ProcessLocation::new("node-0").expect("sender location"),
            ProcessLocation::new("node-1").expect("receiver location"),
        )
        .build()
        .expect_err("remote sender builder must fail");
        assert!(matches!(
            err,
            LavaFlowError::UnsupportedCommunicationScope {
                scope: CommunicationScope::Remote
            }
        ));
    }

    #[test]
    fn receiver_builder_rejects_remote_scope() {
        let err = Builder::receiver(
            ChannelId::new("remote-receiver").expect("channel id"),
            ProcessLocation::new("node-0").expect("receiver location"),
            ProcessLocation::new("node-1").expect("sender location"),
        )
        .build()
        .expect_err("remote receiver builder must fail");
        assert!(matches!(
            err,
            LavaFlowError::UnsupportedCommunicationScope {
                scope: CommunicationScope::Remote
            }
        ));
    }
}
