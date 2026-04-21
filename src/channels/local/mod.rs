use super::{ChannelMetadata, Frame, MessageMeta, MetadataEncoding};
use crate::error::{LavaFlowError, Result};
use crate::memory::allocator::InterprocessMemoryHandle;
use crate::memory::cpu;
use std::convert::TryFrom;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use platform::EndpointAddress;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct ConnectionHeader {
    encoding: MetadataEncoding,
}

impl ConnectionHeader {
    fn from_encoding(encoding: MetadataEncoding) -> Self {
        Self { encoding }
    }

    fn write_to(self, transport: &mut platform::TransportSender) -> Result<()> {
        transport.write_all(&[self.encoding as u8])?;
        transport.flush()
    }

    fn read_from(transport: &mut platform::TransportReceiver) -> Result<Self> {
        let mut encoding = [0_u8; 1];
        transport.read_exact(&mut encoding)?;
        let encoding = MetadataEncoding::try_from(encoding[0])?;
        Ok(Self { encoding })
    }
}

/// Sender half of the local CPU IPC transport.
///
/// This transport uses an OS-native local IPC primitive to transfer the control envelope while the
/// payload bytes stay in shared memory and are referenced through an exported CPU handle.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct CpuSender {
    encoding: MetadataEncoding,
    transport: platform::TransportSender,
}

impl CpuSender {
    fn new(encoding: MetadataEncoding, transport: platform::TransportSender) -> Self {
        Self {
            encoding,
            transport,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn listen(
        encoding: MetadataEncoding,
        address: &EndpointAddress,
    ) -> Result<CpuSenderListener> {
        Ok(CpuSenderListener {
            encoding,
            listener: platform::TransportListener::bind(address)?,
        })
    }

    /// Sends a payload frame with typed metadata through local CPU IPC.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn send<F, M>(&mut self, frame: F, metadata: &M) -> Result<()>
    where
        F: Into<Frame>,
        M: ChannelMetadata,
    {
        let envelope =
            MessageEnvelope::from_frame_and_metadata(frame.into(), self.encoding, metadata)?;
        envelope.write_to(&mut self.transport)
    }

    /// Sends a payload frame with dynamic metadata through local CPU IPC.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn send_map<F>(&mut self, frame: F, metadata: MessageMeta) -> Result<()>
    where
        F: Into<Frame>,
    {
        let envelope =
            MessageEnvelope::from_frame_and_metadata(frame.into(), self.encoding, &metadata)?;
        envelope.write_to(&mut self.transport)
    }
}

#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct CpuSenderListener {
    encoding: MetadataEncoding,
    listener: platform::TransportListener,
}

impl CpuSenderListener {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn accept(self) -> Result<CpuSender> {
        let mut transport = self.listener.accept()?;
        ConnectionHeader::from_encoding(self.encoding).write_to(&mut transport)?;
        Ok(CpuSender::new(self.encoding, transport))
    }
}

/// Receiver half of the local CPU IPC transport.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct CpuReceiver {
    encoding: MetadataEncoding,
    transport: platform::TransportReceiver,
}

impl CpuReceiver {
    fn new(encoding: MetadataEncoding, transport: platform::TransportReceiver) -> Self {
        Self {
            encoding,
            transport,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn connect(address: &EndpointAddress) -> Result<Self> {
        let mut transport = platform::TransportReceiver::connect(address)?;
        let header = ConnectionHeader::read_from(&mut transport)?;
        Ok(Self::new(header.encoding, transport))
    }

    /// Receives a payload frame with typed metadata through local CPU IPC.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn recv<M: ChannelMetadata>(&mut self) -> Result<(Frame, M)> {
        let message = MessageEnvelope::read_from(&mut self.transport)?;
        let metadata = message.decode_metadata(self.encoding)?;
        let frame = message.into_frame()?;
        Ok((frame, metadata))
    }

    /// Receives a payload frame with dynamic metadata through local CPU IPC.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn recv_map(&mut self) -> Result<(Frame, MessageMeta)> {
        let message = MessageEnvelope::read_from(&mut self.transport)?;
        let metadata = message.decode_metadata(self.encoding)?;
        let frame = message.into_frame()?;
        Ok((frame, metadata))
    }
}

#[derive(Debug)]
struct MessageEnvelope {
    size: usize,
    handle: InterprocessMemoryHandle,
    metadata: Vec<u8>,
}

impl MessageEnvelope {
    fn from_frame_and_metadata<M: ChannelMetadata>(
        frame: Frame,
        encoding: MetadataEncoding,
        metadata: &M,
    ) -> Result<Self> {
        let metadata = Self::encode_metadata(encoding, metadata)?;
        let (size, handle) = Self::export_frame(frame)?;
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
        let payload_size =
            u64::try_from(size).map_err(|_| channel_invalid_input("payload size overflow"))?;
        let metadata_len = u32::try_from(metadata.len())
            .map_err(|_| channel_invalid_input("metadata length overflow"))?;

        transport.write_all(&payload_size.to_le_bytes())?;
        transport.send_cpu_handle(handle)?;
        transport.write_all(&metadata_len.to_le_bytes())?;
        transport.write_all(&metadata)?;
        transport.flush()
    }

    fn read_from(transport: &mut platform::TransportReceiver) -> Result<Self> {
        let mut payload_size_bytes = [0_u8; 8];
        transport.read_exact(&mut payload_size_bytes)?;
        let size = usize::try_from(u64::from_le_bytes(payload_size_bytes))
            .map_err(|_| channel_invalid_input("payload size overflow"))?;

        let handle = transport.recv_cpu_handle()?;

        let mut metadata_len_bytes = [0_u8; 4];
        transport.read_exact(&mut metadata_len_bytes)?;
        let metadata_len = usize::try_from(u32::from_le_bytes(metadata_len_bytes))
            .map_err(|_| channel_invalid_input("metadata length overflow"))?;
        let mut metadata = vec![0_u8; metadata_len];
        transport.read_exact(&mut metadata)?;

        Ok(Self {
            size,
            handle,
            metadata,
        })
    }

    fn decode_metadata<M: ChannelMetadata>(&self, encoding: MetadataEncoding) -> Result<M> {
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

    fn into_frame(self) -> Result<Frame> {
        let buffer = cpu::MemoryBuffer::from_shared_handle(self.size, self.handle)?;
        Ok(Frame::Cpu(buffer))
    }

    fn encode_metadata<M: ChannelMetadata>(
        encoding: MetadataEncoding,
        metadata: &M,
    ) -> Result<Vec<u8>> {
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

fn channel_invalid_input(message: &'static str) -> LavaFlowError {
    LavaFlowError::ChannelTransportOperation {
        operation: "decode_envelope",
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::MetaValue;
    use crate::types::ChannelId;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    pub(in crate::channels::local) mod support {
        use super::*;

        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub(in crate::channels::local) struct TestMeta {
            pub(in crate::channels::local) used_size: usize,
            pub(in crate::channels::local) width: u32,
            pub(in crate::channels::local) height: u32,
        }

        impl ChannelMetadata for TestMeta {
            fn used_size(&self) -> usize {
                self.used_size
            }
        }

        pub(in crate::channels::local) const BUFFER_SIZE: usize = 64;
        pub(in crate::channels::local) const USED_SIZE: usize = 17;
        pub(in crate::channels::local) const TEST_BYTE_OFFSET: usize = 7;
        pub(in crate::channels::local) const TEST_BYTE_VALUE: u8 = 0x5a;

        pub(in crate::channels::local) fn test_address() -> EndpointAddress {
            static COUNTER: AtomicU64 = AtomicU64::new(1);

            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let channel_id = ChannelId::new(format!("channel-{id}")).expect("channel id");
            EndpointAddress::from_channel(&channel_id)
        }

        pub(in crate::channels::local) fn test_pair(
            encoding: MetadataEncoding,
        ) -> Result<(CpuSender, CpuReceiver)> {
            let address = test_address();
            let listener = CpuSender::listen(encoding, &address)?;
            let receiver_address = address.clone();
            let receiver_thread = thread::spawn(move || CpuReceiver::connect(&receiver_address));
            let sender = listener.accept()?;
            let receiver = receiver_thread
                .join()
                .expect("receiver connect thread must not panic")?;
            Ok((sender, receiver))
        }

        pub(in crate::channels::local) fn test_allocator() -> cpu::Allocator {
            cpu::Allocator::with_max_allocation_size(usize::MAX)
        }
    }

    use support::{
        BUFFER_SIZE, TEST_BYTE_OFFSET, TEST_BYTE_VALUE, TestMeta, USED_SIZE, test_allocator,
        test_pair,
    };

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
        sender.send(buffer, &metadata).expect("send typed metadata");

        let (frame, received) = receiver.recv::<TestMeta>().expect("receive typed metadata");
        assert_eq!(received, metadata);

        let Frame::Cpu(imported) = frame else {
            panic!("expected cpu frame");
        };
        assert_eq!(imported.size(), BUFFER_SIZE);
        assert_eq!(imported.as_slice()[TEST_BYTE_OFFSET], TEST_BYTE_VALUE);
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
        sender
            .send_map(buffer, metadata.clone())
            .expect("send dynamic metadata");

        let (frame, received) = receiver.recv_map().expect("receive dynamic metadata");
        assert_eq!(received, metadata);
        assert!(matches!(frame, Frame::Cpu(_)));
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
            .send(buffer, &metadata)
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

        let forward = EndpointAddress::from_channel(&first);
        let reverse = EndpointAddress::from_channel(&second);

        assert_ne!(forward, reverse);
    }
}
