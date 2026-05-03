use super::{BuildCancel, Frame, MessageMeta, Metadata, MetadataEncoding};
use crate::error::{LavaFlowError, Result};
use crate::memory::allocator::InterprocessMemoryHandle;
use crate::memory::cpu;
use crate::types::ChannelId;
use std::convert::TryFrom;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use platform::EndpointAddress;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

const LOCAL_PROTOCOL_VERSION: u8 = 1;
const DEFAULT_MAX_LOCAL_CHANNEL_PAYLOAD_SIZE: usize = 1024 * 1024 * 1024;
const DEFAULT_MAX_LOCAL_CHANNEL_METADATA_SIZE: usize = 1024 * 1024;

/// Local IPC peer access policy selected by public builder convenience methods.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Access {
    /// Restrict local IPC to the current Windows logon session or Unix user.
    #[default]
    CurrentSession,
    /// Allow authenticated local OS users to connect to the local IPC endpoint.
    AuthenticatedUsers,
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct ProtocolLimits {
    max_payload_size: usize,
    max_metadata_size: usize,
}

impl ProtocolLimits {
    /// Creates limits with explicit caps.
    ///
    /// A value of `0` for either cap means "use the built-in default" for that dimension.
    pub(crate) fn with_max_sizes(max_payload_size: usize, max_metadata_size: usize) -> Self {
        Self {
            max_payload_size: if max_payload_size == 0 {
                DEFAULT_MAX_LOCAL_CHANNEL_PAYLOAD_SIZE
            } else {
                max_payload_size
            },
            max_metadata_size: if max_metadata_size == 0 {
                DEFAULT_MAX_LOCAL_CHANNEL_METADATA_SIZE
            } else {
                max_metadata_size
            },
        }
    }

    fn validate_outbound_payload_size(&self, payload_size: usize) -> Result<()> {
        if payload_size > self.max_payload_size {
            return Err(channel_protocol_error(
                "write_message_envelope",
                "payload size exceeds configured local channel maximum",
            ));
        }
        Ok(())
    }

    fn validate_outbound_metadata_len(&self, metadata_len: usize) -> Result<()> {
        if metadata_len > self.max_metadata_size {
            return Err(channel_protocol_error(
                "write_message_envelope",
                "metadata length exceeds configured local channel maximum",
            ));
        }
        Ok(())
    }

    fn validate_inbound_payload_size(&self, payload_size: usize) -> Result<()> {
        if payload_size > self.max_payload_size {
            return Err(channel_protocol_error(
                "read_message_envelope",
                "payload size exceeds configured local channel maximum",
            ));
        }
        Ok(())
    }

    fn validate_inbound_metadata_len(&self, metadata_len: usize) -> Result<()> {
        if metadata_len > self.max_metadata_size {
            return Err(channel_protocol_error(
                "read_message_envelope",
                "metadata length exceeds configured local channel maximum",
            ));
        }
        Ok(())
    }
}

impl Default for ProtocolLimits {
    fn default() -> Self {
        Self {
            max_payload_size: DEFAULT_MAX_LOCAL_CHANNEL_PAYLOAD_SIZE,
            max_metadata_size: DEFAULT_MAX_LOCAL_CHANNEL_METADATA_SIZE,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct ConnectionHeader {
    encoding: MetadataEncoding,
}

impl ConnectionHeader {
    fn from_encoding(encoding: MetadataEncoding) -> Self {
        Self { encoding }
    }

    fn write_to(self, transport: &mut platform::TransportSender) -> Result<()> {
        transport.write_all(&[
            ProtocolTag::ConnectionHeader as u8,
            LOCAL_PROTOCOL_VERSION,
            self.encoding as u8,
        ])?;
        transport.flush()
    }

    fn read_from(transport: &mut platform::TransportReceiver) -> Result<Self> {
        let tag = ProtocolTag::read_from_receiver(transport, "read_connection_header")?;
        if tag != ProtocolTag::ConnectionHeader {
            return Err(channel_protocol_error(
                "read_connection_header",
                "unexpected protocol tag",
            ));
        }

        let mut header = [0_u8; 2];
        transport.read_exact(&mut header)?;
        if header[0] != LOCAL_PROTOCOL_VERSION {
            return Err(channel_protocol_error(
                "read_connection_header",
                "unsupported local protocol version",
            ));
        }

        let encoding = MetadataEncoding::try_from(header[1])?;
        Ok(Self { encoding })
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ProtocolTag {
    ConnectionHeader = 1,
    MessageEnvelope = 2,
    ImportOk = 3,
    ImportFailed = 4,
}

impl TryFrom<u8> for ProtocolTag {
    type Error = LavaFlowError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::ConnectionHeader),
            2 => Ok(Self::MessageEnvelope),
            3 => Ok(Self::ImportOk),
            4 => Ok(Self::ImportFailed),
            _ => Err(channel_protocol_error(
                "decode_protocol_tag",
                "unknown protocol tag",
            )),
        }
    }
}

impl ProtocolTag {
    fn read_from_sender(
        transport: &mut platform::TransportSender,
        operation: &'static str,
    ) -> Result<Self> {
        let mut tag = [0_u8; 1];
        transport.read_exact(&mut tag)?;
        Self::try_from(tag[0])
            .map_err(|_| channel_protocol_error(operation, "unknown protocol tag"))
    }

    fn read_from_receiver(
        transport: &mut platform::TransportReceiver,
        operation: &'static str,
    ) -> Result<Self> {
        let mut tag = [0_u8; 1];
        transport.read_exact(&mut tag)?;
        Self::try_from(tag[0])
            .map_err(|_| channel_protocol_error(operation, "unknown protocol tag"))
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum FrameKind {
    Cpu = 1,
    Gpu = 2,
}

impl TryFrom<u8> for FrameKind {
    type Error = LavaFlowError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Cpu),
            2 => Ok(Self::Gpu),
            _ => Err(channel_protocol_error(
                "decode_frame_kind",
                "unknown frame kind",
            )),
        }
    }
}

/// Sender half of the local CPU IPC transport.
///
/// This transport uses an OS-native local IPC primitive to transfer the control envelope while the
/// payload bytes stay in shared memory and are referenced through an exported CPU handle.
#[derive(Debug)]
pub(crate) struct Sender {
    encoding: MetadataEncoding,
    transport: platform::TransportSender,
    limits: ProtocolLimits,
}

impl Sender {
    fn new(
        encoding: MetadataEncoding,
        transport: platform::TransportSender,
        limits: ProtocolLimits,
    ) -> Self {
        Self {
            encoding,
            transport,
            limits,
        }
    }

    pub(crate) fn listen(
        encoding: MetadataEncoding,
        address: &EndpointAddress,
        limits: ProtocolLimits,
        access: Access,
    ) -> Result<SenderListener> {
        Ok(SenderListener {
            encoding,
            listener: platform::TransportListener::bind(address, access)?,
            limits,
        })
    }

    /// Sends a payload frame with typed metadata through local CPU IPC.
    pub(crate) fn send<F, M>(&mut self, frame: F, metadata: &M) -> Result<()>
    where
        F: Into<Frame>,
        M: Metadata,
    {
        let envelope = MessageEnvelope::from_frame_and_metadata(
            frame.into(),
            self.encoding,
            metadata,
            self.limits,
        )?;
        let result = envelope
            .write_to(&mut self.transport)
            .and_then(|()| self.recv_import_ack());
        match result {
            Ok(()) => {
                self.transport.complete_transfer();
                Ok(())
            }
            Err(error) => {
                self.transport.abort_transfer();
                Err(error)
            }
        }
    }

    /// Sends a payload frame with dynamic metadata through local CPU IPC.
    pub(crate) fn send_map<F>(&mut self, frame: F, metadata: MessageMeta) -> Result<()>
    where
        F: Into<Frame>,
    {
        let envelope = MessageEnvelope::from_frame_and_metadata(
            frame.into(),
            self.encoding,
            &metadata,
            self.limits,
        )?;
        let result = envelope
            .write_to(&mut self.transport)
            .and_then(|()| self.recv_import_ack());
        match result {
            Ok(()) => {
                self.transport.complete_transfer();
                Ok(())
            }
            Err(error) => {
                self.transport.abort_transfer();
                Err(error)
            }
        }
    }

    fn recv_import_ack(&mut self) -> Result<()> {
        match ProtocolTag::read_from_sender(&mut self.transport, "read_import_ack")? {
            ProtocolTag::ImportOk => Ok(()),
            ProtocolTag::ImportFailed => Err(channel_protocol_error(
                "read_import_ack",
                "receiver rejected imported handle",
            )),
            _ => Err(channel_protocol_error(
                "read_import_ack",
                "unexpected protocol tag",
            )),
        }
    }
}

pub(crate) fn listen(
    channel_id: &ChannelId,
    encoding: MetadataEncoding,
    limits: ProtocolLimits,
    access: Access,
) -> Result<SenderListener> {
    let address = EndpointAddress::from_channel(channel_id, access);
    Sender::listen(encoding, &address, limits, access)
}

#[derive(Debug)]
pub(crate) struct SenderListener {
    encoding: MetadataEncoding,
    listener: platform::TransportListener,
    limits: ProtocolLimits,
}

impl SenderListener {
    pub(crate) fn accept(self) -> Result<Sender> {
        let mut transport = self.listener.accept()?;
        ConnectionHeader::from_encoding(self.encoding).write_to(&mut transport)?;
        Ok(Sender::new(self.encoding, transport, self.limits))
    }

    pub(crate) fn accept_with_control(
        mut self,
        timeout: Option<Duration>,
        cancel: Option<&BuildCancel>,
        poll_delay: Duration,
        endpoint: &'static str,
    ) -> Result<Sender> {
        let start = Instant::now();

        loop {
            if cancel.is_some_and(BuildCancel::is_cancelled) {
                return Err(LavaFlowError::ChannelBuildCancelled { endpoint });
            }

            match self.listener.try_accept()? {
                Some(mut transport) => {
                    ConnectionHeader::from_encoding(self.encoding).write_to(&mut transport)?;
                    return Ok(Sender::new(self.encoding, transport, self.limits));
                }
                None => {
                    if let Some(timeout) = timeout
                        && start.elapsed() >= timeout
                    {
                        return Err(sender_accept_timeout_error(timeout));
                    }
                    thread::sleep(poll_delay);
                }
            }
        }
    }
}

/// Receiver half of the local CPU IPC transport.
#[derive(Debug)]
pub(crate) struct Receiver {
    encoding: MetadataEncoding,
    limits: ProtocolLimits,
    transport: platform::TransportReceiver,
}

impl Receiver {
    fn new(
        encoding: MetadataEncoding,
        transport: platform::TransportReceiver,
        limits: ProtocolLimits,
    ) -> Self {
        Self {
            encoding,
            transport,
            limits,
        }
    }

    pub(crate) fn connect(address: &EndpointAddress, limits: ProtocolLimits) -> Result<Self> {
        let mut transport = platform::TransportReceiver::connect(address)?;
        let header = ConnectionHeader::read_from(&mut transport)?;
        Ok(Self::new(header.encoding, transport, limits))
    }

    /// Receives a payload frame with typed metadata through local CPU IPC.
    pub(crate) fn recv<M: Metadata>(&mut self) -> Result<(Frame, M)> {
        let message = MessageEnvelope::read_from(&mut self.transport, self.limits)?;
        let frame = match message.try_into_frame() {
            Ok(frame) => {
                self.send_import_ack(ProtocolTag::ImportOk)?;
                frame
            }
            Err(error) => {
                let _ = self.send_import_ack(ProtocolTag::ImportFailed);
                return Err(error);
            }
        };
        let metadata = message.decode_metadata(self.encoding)?;
        Ok((frame, metadata))
    }

    /// Receives a payload frame with dynamic metadata through local CPU IPC.
    pub(crate) fn recv_map(&mut self) -> Result<(Frame, MessageMeta)> {
        let message = MessageEnvelope::read_from(&mut self.transport, self.limits)?;
        let frame = match message.try_into_frame() {
            Ok(frame) => {
                self.send_import_ack(ProtocolTag::ImportOk)?;
                frame
            }
            Err(error) => {
                let _ = self.send_import_ack(ProtocolTag::ImportFailed);
                return Err(error);
            }
        };
        let metadata = message.decode_metadata(self.encoding)?;
        Ok((frame, metadata))
    }

    fn send_import_ack(&mut self, tag: ProtocolTag) -> Result<()> {
        self.transport.write_all(&[tag as u8])?;
        self.transport.flush()
    }
}

pub(crate) fn connect(
    channel_id: &ChannelId,
    limits: ProtocolLimits,
    access: Access,
) -> Result<Receiver> {
    let address = EndpointAddress::from_channel(channel_id, access);
    Receiver::connect(&address, limits)
}

pub(crate) fn is_retryable_connect_error(err: &LavaFlowError) -> bool {
    match err {
        LavaFlowError::ChannelTransportOperation { operation, source } => {
            platform::is_retryable_connect_error(operation, source)
        }
        _ => false,
    }
}

#[derive(Debug)]
struct MessageEnvelope {
    size: usize,
    handle: InterprocessMemoryHandle,
    metadata: Vec<u8>,
}

impl MessageEnvelope {
    fn from_frame_and_metadata<M: Metadata>(
        frame: Frame,
        encoding: MetadataEncoding,
        metadata: &M,
        limits: ProtocolLimits,
    ) -> Result<Self> {
        let metadata = Self::encode_metadata(encoding, metadata)?;
        let (size, handle) = Self::export_frame(frame)?;
        limits.validate_outbound_payload_size(size)?;
        limits.validate_outbound_metadata_len(metadata.len())?;
        Ok(Self {
            size,
            handle,
            metadata,
        })
    }

    fn write_to(self, transport: &mut platform::TransportSender) -> Result<()> {
        let Self {
            size,
            handle,
            metadata,
        } = self;
        let payload_size = size as u64;
        let metadata_len = u32::try_from(metadata.len()).map_err(|_| {
            channel_protocol_error("write_message_envelope", "metadata length overflow")
        })?;
        let kind = FrameKind::from_handle(&handle);

        // TODO: add a generic send_handle() dispatch here once local GPU handle transfer is
        // implemented. That generic entry point should select backend-specific
        // send_cpu_handle()/send_gpu_handle() methods because the rights and transfer mechanics
        // differ by handle kind and platform.
        transport.write_all(&[ProtocolTag::MessageEnvelope as u8, kind as u8])?;
        transport.write_all(&payload_size.to_le_bytes())?;
        transport.send_cpu_handle(handle)?;
        transport.write_all(&metadata_len.to_le_bytes())?;
        transport.write_all(&metadata)?;
        transport.flush()
    }

    fn read_from(
        transport: &mut platform::TransportReceiver,
        limits: ProtocolLimits,
    ) -> Result<Self> {
        let tag = ProtocolTag::read_from_receiver(transport, "read_message_envelope")?;
        if tag != ProtocolTag::MessageEnvelope {
            return Err(channel_protocol_error(
                "read_message_envelope",
                "unexpected protocol tag",
            ));
        }

        let mut kind = [0_u8; 1];
        transport.read_exact(&mut kind)?;
        let kind = FrameKind::try_from(kind[0])?;

        let mut payload_size_bytes = [0_u8; 8];
        transport.read_exact(&mut payload_size_bytes)?;
        let size = usize::try_from(u64::from_le_bytes(payload_size_bytes)).map_err(|_| {
            channel_protocol_error("read_message_envelope", "payload size overflow")
        })?;
        limits.validate_inbound_payload_size(size)?;

        // TODO: add a generic recv_handle() dispatch here once local GPU handle transfer is
        // implemented. The generic path should validate the frame kind tag and then call
        // recv_cpu_handle()/recv_gpu_handle() as appropriate for the transferred handle class.
        let handle = transport.recv_cpu_handle()?;
        if FrameKind::from_handle(&handle) != kind {
            return Err(channel_protocol_error(
                "read_message_envelope",
                "frame kind does not match transferred handle",
            ));
        }

        let mut metadata_len_bytes = [0_u8; 4];
        transport.read_exact(&mut metadata_len_bytes)?;
        let metadata_len = u32::from_le_bytes(metadata_len_bytes) as usize;
        limits.validate_inbound_metadata_len(metadata_len)?;
        let mut metadata = vec![0_u8; metadata_len];
        transport.read_exact(&mut metadata)?;

        Ok(Self {
            size,
            handle,
            metadata,
        })
    }

    fn decode_metadata<M: Metadata>(&self, encoding: MetadataEncoding) -> Result<M> {
        match encoding {
            MetadataEncoding::Json => serde_json::from_slice(&self.metadata).map_err(|source| {
                LavaFlowError::ChannelMetadataCodec {
                    operation: "deserialize_metadata",
                    source,
                }
            }),
            MetadataEncoding::Cbor => {
                Err(LavaFlowError::UnsupportedMetadataEncoding { encoding: "cbor" })
            }
        }
    }

    fn try_into_frame(&self) -> Result<Frame> {
        // TODO: import GPU-backed frames here once the local transport grows recv_gpu_handle()
        // support and the Vulkan IPC path is wired into channels::local.
        match FrameKind::from_handle(&self.handle) {
            FrameKind::Cpu => {
                let buffer =
                    cpu::MemoryBuffer::from_shared_handle(self.size, self.handle.try_clone()?)?;
                Ok(Frame::Cpu(buffer))
            }
            FrameKind::Gpu => Err(LavaFlowError::UnsupportedChannelBufferKind { kind: "gpu" }),
        }
    }

    fn encode_metadata<M: Metadata>(encoding: MetadataEncoding, metadata: &M) -> Result<Vec<u8>> {
        match encoding {
            MetadataEncoding::Json => {
                serde_json::to_vec(metadata).map_err(|source| LavaFlowError::ChannelMetadataCodec {
                    operation: "serialize_metadata",
                    source,
                })
            }
            MetadataEncoding::Cbor => {
                Err(LavaFlowError::UnsupportedMetadataEncoding { encoding: "cbor" })
            }
        }
    }

    fn export_frame(frame: Frame) -> Result<(usize, InterprocessMemoryHandle)> {
        // TODO: export GPU-backed frames here once the generic local transport grows a
        // send_gpu_handle() path. For now channels::local remains CPU-only at the handle-transfer
        // layer even though the protocol already reserves a GPU frame-kind tag.
        match frame {
            Frame::Cpu(buffer) => Ok((buffer.size(), buffer.shared_handle()?)),
            Frame::Gpu(_) => Err(LavaFlowError::UnsupportedChannelBufferKind { kind: "gpu" }),
        }
    }
}

fn channel_transport_error(operation: &'static str, source: std::io::Error) -> LavaFlowError {
    match source.kind() {
        std::io::ErrorKind::BrokenPipe
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::NotConnected
        | std::io::ErrorKind::UnexpectedEof => LavaFlowError::ChannelDisconnected,
        _ => LavaFlowError::ChannelTransportOperation { operation, source },
    }
}

fn channel_protocol_error(operation: &'static str, message: &'static str) -> LavaFlowError {
    LavaFlowError::ChannelTransportOperation {
        operation,
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, message),
    }
}

fn sender_accept_timeout_error(timeout: Duration) -> LavaFlowError {
    LavaFlowError::ChannelTransportOperation {
        operation: "accept",
        source: std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("sender accept timed out after {timeout:?}"),
        ),
    }
}

#[cfg(test)]
pub(crate) fn stable_test_runtime_dir_guard(
    test_name: &str,
    id: u64,
) -> platform::TestRuntimeDirGuard {
    platform::stable_test_runtime_dir_guard(test_name, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::MetaValue;
    use serde::{Deserialize, Serialize, Serializer};
    use std::collections::BTreeMap;
    use std::thread;

    pub(in crate::channel::local) mod support {
        use super::*;

        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub(in crate::channel::local) struct TestMeta {
            pub(in crate::channel::local) used_size: usize,
            pub(in crate::channel::local) width: u32,
            pub(in crate::channel::local) height: u32,
        }

        impl Metadata for TestMeta {
            fn used_size(&self) -> usize {
                self.used_size
            }
        }

        #[derive(Clone, Debug, Deserialize)]
        pub(in crate::channel::local) struct FailingMeta;

        impl Metadata for FailingMeta {
            fn used_size(&self) -> usize {
                0
            }
        }

        impl Serialize for FailingMeta {
            fn serialize<S>(&self, _serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                Err(serde::ser::Error::custom("failing metadata serializer"))
            }
        }

        pub(in crate::channel::local) const BUFFER_SIZE: usize = 64;
        pub(in crate::channel::local) const USED_SIZE: usize = 17;
        pub(in crate::channel::local) const TEST_BYTE_OFFSET: usize = 7;
        pub(in crate::channel::local) const TEST_BYTE_VALUE: u8 = 0x5a;

        pub(in crate::channel::local) fn test_address() -> EndpointAddress {
            platform::tests::support::test_address()
        }

        pub(in crate::channel::local) fn test_pair(
            encoding: MetadataEncoding,
        ) -> Result<(Sender, Receiver)> {
            let address = test_address();
            let listener = Sender::listen(
                encoding,
                &address,
                ProtocolLimits::default(),
                Access::CurrentSession,
            )?;
            let receiver_address = address.clone();
            let receiver_thread = thread::spawn(move || {
                Receiver::connect(&receiver_address, ProtocolLimits::default())
            });
            let sender = listener.accept()?;
            let receiver = receiver_thread
                .join()
                .expect("receiver connect thread must not panic")?;
            Ok((sender, receiver))
        }

        pub(in crate::channel::local) fn test_transport_pair()
        -> Result<(platform::TransportSender, platform::TransportReceiver)> {
            let address = test_address();
            let listener = platform::TransportListener::bind(&address, Access::CurrentSession)?;
            let receiver_address = address.clone();
            let receiver_thread =
                thread::spawn(move || platform::TransportReceiver::connect(&receiver_address));
            let sender = listener.accept()?;
            let receiver = receiver_thread
                .join()
                .expect("receiver connect thread must not panic")?;
            Ok((sender, receiver))
        }

        pub(in crate::channel::local) fn test_allocator() -> cpu::Allocator {
            cpu::Allocator::with_max_allocation_size(usize::MAX)
        }
    }

    use support::{
        BUFFER_SIZE, FailingMeta, TEST_BYTE_OFFSET, TEST_BYTE_VALUE, TestMeta, USED_SIZE,
        test_allocator, test_pair, test_transport_pair,
    };

    const SMALL_PAYLOAD_LIMIT: usize = BUFFER_SIZE - 1;
    const SMALL_METADATA_LIMIT: usize = 8;

    #[test]
    fn cpu_local_ipc_round_trips_typed_metadata_and_shared_payload() {
        let (mut sender, mut receiver) =
            test_pair(MetadataEncoding::Json).expect("create cpu local ipc pair");
        let mut buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        buffer.as_mut_slice()[TEST_BYTE_OFFSET] = TEST_BYTE_VALUE;

        let metadata = TestMeta {
            used_size: USED_SIZE,
            width: 1920,
            height: 1080,
        };
        // send() waits for the receiver-side import ACK, so recv() must run concurrently.
        let recv_thread = thread::spawn(move || {
            let (frame, received) = receiver.recv::<TestMeta>().expect("receive typed metadata");
            let Frame::Cpu(imported) = frame else {
                panic!("expected cpu frame");
            };
            (
                received,
                imported.size(),
                imported.as_slice()[TEST_BYTE_OFFSET],
            )
        });

        sender.send(buffer, &metadata).expect("send typed metadata");

        let (received, imported_size, imported_value) =
            recv_thread.join().expect("receiver thread must not panic");
        assert_eq!(received, metadata);
        assert_eq!(imported_size, BUFFER_SIZE);
        assert_eq!(imported_value, TEST_BYTE_VALUE);
    }

    #[test]
    fn cpu_local_ipc_round_trips_dynamic_metadata() {
        let (mut sender, mut receiver) =
            test_pair(MetadataEncoding::Json).expect("create cpu local ipc pair");
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");

        let mut values = BTreeMap::new();
        values.insert("epoch".to_string(), MetaValue::U64(42));
        values.insert("label".to_string(), MetaValue::String("tile-0".to_string()));
        let metadata = MessageMeta {
            used_size: USED_SIZE,
            values,
        };
        // send_map() also blocks on the receiver-side import ACK, so recv_map() must run concurrently.
        let recv_thread = thread::spawn(move || {
            let (frame, received) = receiver.recv_map().expect("receive dynamic metadata");
            assert!(matches!(frame, Frame::Cpu(_)));
            received
        });

        sender
            .send_map(buffer, metadata.clone())
            .expect("send dynamic metadata");

        let received = recv_thread.join().expect("receiver thread must not panic");
        assert_eq!(received, metadata);
    }

    #[test]
    fn receiver_reports_disconnect_when_sender_is_dropped() {
        let (sender, mut receiver) =
            test_pair(MetadataEncoding::Json).expect("create cpu local ipc pair");
        drop(sender);

        let err = receiver
            .recv_map()
            .expect_err("recv must fail after sender disconnect");
        assert!(matches!(err, LavaFlowError::ChannelDisconnected));
    }

    #[test]
    fn sender_reports_disconnect_when_receiver_is_dropped() {
        let (mut sender, receiver) =
            test_pair(MetadataEncoding::Json).expect("create cpu local ipc pair");
        drop(receiver);

        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let metadata = TestMeta {
            used_size: USED_SIZE,
            width: 16,
            height: 16,
        };

        let err = sender
            .send(buffer, &metadata)
            .expect_err("send must fail after receiver disconnect");
        assert!(matches!(err, LavaFlowError::ChannelDisconnected));
    }

    #[test]
    fn send_map_reports_disconnect_when_receiver_is_dropped() {
        let (mut sender, receiver) =
            test_pair(MetadataEncoding::Json).expect("create cpu local ipc pair");
        drop(receiver);

        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let metadata = MessageMeta {
            used_size: USED_SIZE,
            values: BTreeMap::new(),
        };

        let err = sender
            .send_map(buffer, metadata)
            .expect_err("send_map must fail after receiver disconnect");
        assert!(matches!(err, LavaFlowError::ChannelDisconnected));
    }

    #[test]
    fn cbor_metadata_encoding_is_not_implemented_yet() {
        let (mut sender, _) = test_pair(MetadataEncoding::Cbor).expect("create cpu local ipc pair");
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let metadata = TestMeta {
            used_size: USED_SIZE,
            width: 1,
            height: 1,
        };

        let err = sender
            .send(buffer, &metadata)
            .expect_err("cbor should be rejected in local cpu ipc");
        assert!(matches!(
            err,
            LavaFlowError::UnsupportedMetadataEncoding { encoding: "cbor" }
        ));
    }

    #[test]
    fn gpu_frames_are_not_supported_by_cpu_local_ipc() {
        let (mut sender, _) = test_pair(MetadataEncoding::Json).expect("create cpu local ipc pair");
        let gpu_allocator = match crate::memory::gpu::Allocator::new() {
            Ok(allocator) => allocator,
            Err(_) => return,
        };
        let buffer = gpu_allocator
            .allocate(BUFFER_SIZE)
            .expect("allocate gpu payload");
        let metadata = TestMeta {
            used_size: USED_SIZE,
            width: 16,
            height: 16,
        };

        let err = sender
            .send(Frame::Gpu(buffer), &metadata)
            .expect_err("cpu local ipc should reject gpu frames");
        assert!(matches!(
            err,
            LavaFlowError::UnsupportedChannelBufferKind { kind: "gpu" }
        ));
    }

    #[test]
    fn local_endpoint_address_differs_by_channel_id() {
        let first = ChannelId::new("channel-0").expect("first channel id");
        let second = ChannelId::new("channel-1").expect("second channel id");

        let forward = EndpointAddress::from_channel(&first, Access::CurrentSession);
        let reverse = EndpointAddress::from_channel(&second, Access::CurrentSession);

        assert_ne!(forward, reverse);
    }

    #[test]
    fn test_meta_used_size_returns_stored_value() {
        let metadata = TestMeta {
            used_size: USED_SIZE,
            width: 1,
            height: 1,
        };

        assert_eq!(metadata.used_size(), USED_SIZE);
    }

    #[test]
    fn failing_meta_used_size_returns_zero() {
        assert_eq!(FailingMeta.used_size(), 0);
    }

    #[test]
    fn local_protocol_limits_default_to_expected_caps() {
        let limits = ProtocolLimits::default();
        assert_eq!(
            limits.max_payload_size,
            DEFAULT_MAX_LOCAL_CHANNEL_PAYLOAD_SIZE
        );
        assert_eq!(
            limits.max_metadata_size,
            DEFAULT_MAX_LOCAL_CHANNEL_METADATA_SIZE
        );
    }

    #[test]
    fn local_protocol_limits_zero_caps_fall_back_to_defaults() {
        let limits = ProtocolLimits::with_max_sizes(0, 0);
        assert_eq!(
            limits.max_payload_size,
            DEFAULT_MAX_LOCAL_CHANNEL_PAYLOAD_SIZE
        );
        assert_eq!(
            limits.max_metadata_size,
            DEFAULT_MAX_LOCAL_CHANNEL_METADATA_SIZE
        );
    }

    #[test]
    fn receiver_rejects_unsupported_protocol_version() {
        let address = support::test_address();
        let listener = platform::TransportListener::bind(&address, Access::CurrentSession)
            .expect("bind transport");
        let receiver_address = address.clone();
        let receiver_thread =
            thread::spawn(move || Receiver::connect(&receiver_address, ProtocolLimits::default()));

        let mut sender_transport = listener.accept().expect("accept transport");
        sender_transport
            .write_all(&[
                ProtocolTag::ConnectionHeader as u8,
                LOCAL_PROTOCOL_VERSION + 1,
                MetadataEncoding::Json as u8,
            ])
            .expect("write invalid connection header");
        sender_transport.flush().expect("flush invalid header");

        let err = receiver_thread
            .join()
            .expect("receiver connect thread must not panic")
            .expect_err("receiver must reject unsupported protocol version");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_connection_header",
                ..
            }
        ));
    }

    #[test]
    fn receiver_rejects_unexpected_connection_header_tag() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8])
            .expect("write unexpected tag");
        sender_transport.flush().expect("flush unexpected tag");

        let err = ConnectionHeader::read_from(&mut receiver_transport)
            .expect_err("receiver must reject unexpected connection-header tag");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_connection_header",
                ..
            }
        ));
    }

    #[test]
    fn receiver_rejects_unknown_metadata_encoding_in_connection_header() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[
                ProtocolTag::ConnectionHeader as u8,
                LOCAL_PROTOCOL_VERSION,
                99,
            ])
            .expect("write invalid connection header");
        sender_transport
            .flush()
            .expect("flush invalid connection header");

        let err = ConnectionHeader::read_from(&mut receiver_transport)
            .expect_err("receiver must reject unknown metadata encoding");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "decode_connection_header",
                ..
            }
        ));
    }

    #[test]
    fn sender_reports_import_failed_ack() {
        let (sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let mut sender = Sender::new(
            MetadataEncoding::Json,
            sender_transport,
            ProtocolLimits::default(),
        );
        receiver_transport
            .write_all(&[ProtocolTag::ImportFailed as u8])
            .expect("write import-failed ack");
        receiver_transport.flush().expect("flush import-failed ack");

        let err = sender
            .recv_import_ack()
            .expect_err("sender must report rejected imported handle");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_import_ack",
                ..
            }
        ));
    }

    #[test]
    fn protocol_tag_reads_sender_import_ack_directly() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        receiver_transport
            .write_all(&[ProtocolTag::ImportOk as u8])
            .expect("write import-ok tag");
        receiver_transport.flush().expect("flush import-ok tag");

        let tag = ProtocolTag::read_from_sender(&mut sender_transport, "read_import_ack")
            .expect("read import-ok tag directly");
        assert_eq!(tag, ProtocolTag::ImportOk);
    }

    #[test]
    fn sender_rejects_unexpected_import_ack_tag() {
        let (sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let mut sender = Sender::new(
            MetadataEncoding::Json,
            sender_transport,
            ProtocolLimits::default(),
        );
        receiver_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8])
            .expect("write unexpected ack tag");
        receiver_transport
            .flush()
            .expect("flush unexpected ack tag");

        let err = sender
            .recv_import_ack()
            .expect_err("sender must reject unexpected ack tag");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_import_ack",
                ..
            }
        ));
    }

    #[test]
    fn receiver_rejects_unknown_protocol_tag_when_reading_message_envelope() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[99])
            .expect("write unknown protocol tag");
        sender_transport
            .flush()
            .expect("flush unknown protocol tag");

        let err = MessageEnvelope::read_from(&mut receiver_transport, ProtocolLimits::default())
            .expect_err("receiver must reject unknown protocol tag");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_message_envelope",
                ..
            }
        ));
    }

    #[test]
    fn receiver_rejects_unexpected_but_known_protocol_tag_when_reading_message_envelope() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[ProtocolTag::ImportOk as u8])
            .expect("write unexpected known protocol tag");
        sender_transport
            .flush()
            .expect("flush unexpected known protocol tag");

        let err = MessageEnvelope::read_from(&mut receiver_transport, ProtocolLimits::default())
            .expect_err("receiver must reject unexpected known message tag");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_message_envelope",
                ..
            }
        ));
    }

    #[test]
    fn receiver_rejects_unknown_frame_kind_in_message_envelope() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8, 99])
            .expect("write message tag with unknown frame kind");
        sender_transport
            .flush()
            .expect("flush message tag with unknown frame kind");

        let err = MessageEnvelope::read_from(&mut receiver_transport, ProtocolLimits::default())
            .expect_err("receiver must reject unknown frame kind");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "decode_frame_kind",
                ..
            }
        ));
    }

    #[test]
    fn receiver_rejects_frame_kind_that_does_not_match_transferred_handle() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let handle = buffer.shared_handle().expect("export shared handle");

        sender_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8, FrameKind::Gpu as u8])
            .expect("write message tag and mismatched frame kind");
        sender_transport
            .write_all(&(BUFFER_SIZE as u64).to_le_bytes())
            .expect("write payload size");
        sender_transport
            .send_cpu_handle(handle)
            .expect("send cpu handle");
        sender_transport
            .write_all(&0_u32.to_le_bytes())
            .expect("write metadata length");
        sender_transport.flush().expect("flush mismatched message");

        let err = MessageEnvelope::read_from(&mut receiver_transport, ProtocolLimits::default())
            .expect_err("receiver must reject frame kind / handle mismatch");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_message_envelope",
                ..
            }
        ));
    }

    #[test]
    fn message_envelope_reports_json_metadata_decode_errors() {
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let handle = buffer.shared_handle().expect("export shared handle");
        let envelope = MessageEnvelope {
            size: BUFFER_SIZE,
            handle,
            metadata: b"{".to_vec(),
        };

        let err = envelope
            .decode_metadata::<TestMeta>(MetadataEncoding::Json)
            .expect_err("invalid json metadata must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelMetadataCodec {
                operation: "deserialize_metadata",
                ..
            }
        ));
    }

    #[test]
    fn message_envelope_rejects_cbor_metadata_decode() {
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let handle = buffer.shared_handle().expect("export shared handle");
        let envelope = MessageEnvelope {
            size: BUFFER_SIZE,
            handle,
            metadata: Vec::new(),
        };

        let err = envelope
            .decode_metadata::<TestMeta>(MetadataEncoding::Cbor)
            .expect_err("cbor metadata decode must be rejected");
        assert!(matches!(
            err,
            LavaFlowError::UnsupportedMetadataEncoding { encoding: "cbor" }
        ));
    }

    #[test]
    fn message_envelope_reports_json_metadata_encode_errors() {
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");

        let err = MessageEnvelope::from_frame_and_metadata(
            Frame::Cpu(buffer),
            MetadataEncoding::Json,
            &FailingMeta,
            ProtocolLimits::default(),
        )
        .expect_err("failing serializer must surface metadata encode error");
        assert!(matches!(
            err,
            LavaFlowError::ChannelMetadataCodec {
                operation: "serialize_metadata",
                ..
            }
        ));
    }

    #[test]
    fn message_envelope_direct_json_encode_decode_supports_typed_and_dynamic_metadata() {
        let typed_metadata = TestMeta {
            used_size: USED_SIZE,
            width: 320,
            height: 240,
        };
        let typed_bytes = MessageEnvelope::encode_metadata(MetadataEncoding::Json, &typed_metadata)
            .expect("encode typed metadata");
        let typed_handle = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate typed payload")
            .shared_handle()
            .expect("export typed shared handle");
        let typed_envelope = MessageEnvelope {
            size: BUFFER_SIZE,
            handle: typed_handle,
            metadata: typed_bytes,
        };
        let decoded_typed = typed_envelope
            .decode_metadata::<TestMeta>(MetadataEncoding::Json)
            .expect("decode typed metadata");
        assert_eq!(decoded_typed, typed_metadata);

        let dynamic_metadata = MessageMeta {
            used_size: USED_SIZE,
            values: BTreeMap::from([("width".into(), MetaValue::F64(320.0))]),
        };
        let dynamic_bytes =
            MessageEnvelope::encode_metadata(MetadataEncoding::Json, &dynamic_metadata)
                .expect("encode dynamic metadata");
        let dynamic_handle = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate dynamic payload")
            .shared_handle()
            .expect("export dynamic shared handle");
        let dynamic_envelope = MessageEnvelope {
            size: BUFFER_SIZE,
            handle: dynamic_handle,
            metadata: dynamic_bytes,
        };
        let decoded_dynamic = dynamic_envelope
            .decode_metadata::<MessageMeta>(MetadataEncoding::Json)
            .expect("decode dynamic metadata");
        assert_eq!(decoded_dynamic, dynamic_metadata);
    }

    #[test]
    fn message_envelope_write_and_read_round_trip_directly() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let metadata = TestMeta {
            used_size: USED_SIZE,
            width: 123,
            height: 456,
        };
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let envelope = MessageEnvelope::from_frame_and_metadata(
            Frame::Cpu(buffer),
            MetadataEncoding::Json,
            &metadata,
            ProtocolLimits::default(),
        )
        .expect("create message envelope");

        envelope
            .write_to(&mut sender_transport)
            .expect("write message envelope directly");
        sender_transport.complete_transfer();
        let read_back =
            MessageEnvelope::read_from(&mut receiver_transport, ProtocolLimits::default())
                .expect("read message envelope");

        let decoded = read_back
            .decode_metadata::<TestMeta>(MetadataEncoding::Json)
            .expect("decode written metadata");
        assert_eq!(decoded, metadata);
        assert_eq!(read_back.size, BUFFER_SIZE);
        assert!(matches!(
            FrameKind::from_handle(&read_back.handle),
            FrameKind::Cpu
        ));
    }

    #[test]
    fn message_envelope_write_rejects_payloads_above_configured_limit() {
        let limits = ProtocolLimits::with_max_sizes(
            SMALL_PAYLOAD_LIMIT,
            DEFAULT_MAX_LOCAL_CHANNEL_METADATA_SIZE,
        );
        let metadata = TestMeta {
            used_size: USED_SIZE,
            width: 1,
            height: 1,
        };
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let err = MessageEnvelope::from_frame_and_metadata(
            Frame::Cpu(buffer),
            MetadataEncoding::Json,
            &metadata,
            limits,
        )
        .expect_err("oversized payload must be rejected before send");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "write_message_envelope",
                ..
            }
        ));
    }

    #[test]
    fn message_envelope_write_rejects_metadata_above_configured_limit() {
        let limits = ProtocolLimits::with_max_sizes(
            DEFAULT_MAX_LOCAL_CHANNEL_PAYLOAD_SIZE,
            SMALL_METADATA_LIMIT,
        );
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let metadata = MessageMeta {
            used_size: USED_SIZE,
            values: BTreeMap::from([(
                "blob".into(),
                MetaValue::Bytes(vec![0_u8; SMALL_METADATA_LIMIT + 1]),
            )]),
        };

        let err = MessageEnvelope::from_frame_and_metadata(
            Frame::Cpu(buffer),
            MetadataEncoding::Json,
            &metadata,
            limits,
        )
        .expect_err("oversized metadata must be rejected before send");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "write_message_envelope",
                ..
            }
        ));
    }

    #[test]
    fn receiver_rejects_payload_size_above_configured_limit_when_reading_message_envelope() {
        let limits = ProtocolLimits::with_max_sizes(
            SMALL_PAYLOAD_LIMIT,
            DEFAULT_MAX_LOCAL_CHANNEL_METADATA_SIZE,
        );
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8, FrameKind::Cpu as u8])
            .expect("write message tag");
        sender_transport
            .write_all(&(BUFFER_SIZE as u64).to_le_bytes())
            .expect("write oversized payload size");
        sender_transport
            .flush()
            .expect("flush oversized payload size");

        let err = MessageEnvelope::read_from(&mut receiver_transport, limits)
            .expect_err("receiver must reject oversized payload size");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_message_envelope",
                ..
            }
        ));
    }

    #[test]
    fn receiver_rejects_metadata_length_above_configured_limit_when_reading_message_envelope() {
        let limits = ProtocolLimits::with_max_sizes(
            DEFAULT_MAX_LOCAL_CHANNEL_PAYLOAD_SIZE,
            SMALL_METADATA_LIMIT,
        );
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let handle = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload")
            .shared_handle()
            .expect("export shared handle");
        sender_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8, FrameKind::Cpu as u8])
            .expect("write message tag");
        sender_transport
            .write_all(&(BUFFER_SIZE as u64).to_le_bytes())
            .expect("write payload size");
        sender_transport
            .send_cpu_handle(handle)
            .expect("send cpu handle");
        sender_transport
            .write_all(&((SMALL_METADATA_LIMIT + 1) as u32).to_le_bytes())
            .expect("write oversized metadata length");
        sender_transport
            .flush()
            .expect("flush oversized metadata length");

        let err = MessageEnvelope::read_from(&mut receiver_transport, limits)
            .expect_err("receiver must reject oversized metadata length");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_message_envelope",
                ..
            }
        ));
    }

    #[test]
    fn message_envelope_rejects_gpu_handle_frame_import_in_cpu_transport() {
        let gpu_allocator = match crate::memory::gpu::Allocator::new() {
            Ok(allocator) => allocator,
            Err(_) => return,
        };
        let buffer = gpu_allocator
            .allocate(BUFFER_SIZE)
            .expect("allocate gpu payload");
        let handle = buffer.shared_handle().expect("export gpu shared handle");
        let envelope = MessageEnvelope {
            size: BUFFER_SIZE,
            handle,
            metadata: Vec::new(),
        };

        let err = envelope
            .try_into_frame()
            .expect_err("cpu local transport must reject gpu handle import");
        assert!(matches!(
            err,
            LavaFlowError::UnsupportedChannelBufferKind { kind: "gpu" }
        ));
    }

    #[cfg(windows)]
    #[test]
    fn recv_reports_import_failure_and_sends_import_failed_ack_for_invalid_cpu_handle() {
        let (mut sender_transport, receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let mut receiver = Receiver::new(
            MetadataEncoding::Json,
            receiver_transport,
            ProtocolLimits::default(),
        );
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let handle = buffer.shared_handle().expect("export shared handle");
        let metadata = serde_json::to_vec(&TestMeta {
            used_size: USED_SIZE,
            width: 8,
            height: 8,
        })
        .expect("serialize metadata");

        sender_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8, FrameKind::Cpu as u8])
            .expect("write message tag");
        sender_transport
            .write_all(&(BUFFER_SIZE as u64).to_le_bytes())
            .expect("write payload size");
        sender_transport
            .send_cpu_handle(handle)
            .expect("send cpu handle");
        // Close the transferred handle before the receiver imports it so recv() exercises the
        // ImportFailed path instead of failing earlier at envelope parsing time.
        sender_transport.abort_transfer();
        sender_transport
            .write_all(&(metadata.len() as u32).to_le_bytes())
            .expect("write metadata length");
        sender_transport
            .write_all(&metadata)
            .expect("write metadata bytes");
        sender_transport.flush().expect("flush message");

        let err = receiver
            .recv::<TestMeta>()
            .expect_err("receiver must fail when transferred cpu handle is closed");
        assert!(matches!(err, LavaFlowError::SharedMemoryOperation { .. }));

        let ack = ProtocolTag::read_from_sender(&mut sender_transport, "read_import_ack")
            .expect("read import-failed ack");
        assert_eq!(ack, ProtocolTag::ImportFailed);
    }

    #[cfg(windows)]
    #[test]
    fn recv_map_reports_import_failure_and_sends_import_failed_ack_for_invalid_cpu_handle() {
        let (mut sender_transport, receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let mut receiver = Receiver::new(
            MetadataEncoding::Json,
            receiver_transport,
            ProtocolLimits::default(),
        );
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let handle = buffer.shared_handle().expect("export shared handle");
        let metadata = serde_json::to_vec(&MessageMeta {
            used_size: USED_SIZE,
            values: BTreeMap::new(),
        })
        .expect("serialize metadata");

        sender_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8, FrameKind::Cpu as u8])
            .expect("write message tag");
        sender_transport
            .write_all(&(BUFFER_SIZE as u64).to_le_bytes())
            .expect("write payload size");
        sender_transport
            .send_cpu_handle(handle)
            .expect("send cpu handle");
        sender_transport.abort_transfer();
        sender_transport
            .write_all(&(metadata.len() as u32).to_le_bytes())
            .expect("write metadata length");
        sender_transport
            .write_all(&metadata)
            .expect("write metadata bytes");
        sender_transport.flush().expect("flush message");

        let err = receiver
            .recv_map()
            .expect_err("receiver must fail when transferred cpu handle is closed");
        assert!(matches!(err, LavaFlowError::SharedMemoryOperation { .. }));

        let ack = ProtocolTag::read_from_sender(&mut sender_transport, "read_import_ack")
            .expect("read import-failed ack");
        assert_eq!(ack, ProtocolTag::ImportFailed);
    }
}
