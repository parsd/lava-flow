use super::{BuildCancel, Frame, MessageMeta, Metadata, MetadataEncoding};
use crate::error::{LavaFlowError, Result};
use crate::memory::allocator::InterprocessMemoryHandle;
use crate::memory::{cpu, gpu};
use crate::types::ChannelId;
use std::convert::TryFrom;
use std::thread;
use std::time::{Duration, Instant};

mod auth;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

use auth::{AuthSetupError, SharedSecret};
use platform::EndpointAddress;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

const LOCAL_PROTOCOL_VERSION: u8 = 1;
const DEFAULT_MAX_LOCAL_CHANNEL_PAYLOAD_SIZE: usize = 1024 * 1024 * 1024;
const DEFAULT_MAX_LOCAL_CHANNEL_METADATA_SIZE: usize = 1024 * 1024;

/// Local IPC peer access policy selected by public builder convenience methods.
#[repr(u8)]
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Access {
    /// Restrict local IPC to the current Windows logon session or Unix user.
    #[default]
    CurrentSession = 1,
    /// Allow authenticated local OS users to connect to the local IPC endpoint.
    AuthenticatedUsers = 2,
}

impl TryFrom<u8> for Access {
    type Error = LavaFlowError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::CurrentSession),
            2 => Ok(Self::AuthenticatedUsers),
            _ => Err(channel_protocol_error(
                "decode_local_access",
                "unknown local access policy",
            )),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AuthOptions {
    shared_secret: Option<std::result::Result<SharedSecret, AuthSetupError>>,
    expected_peer_process_id: Option<u32>,
}

impl AuthOptions {
    pub(crate) fn with_shared_secret(mut self, secret: Vec<u8>) -> Self {
        self.shared_secret = Some(SharedSecret::new(secret));
        self
    }

    pub(crate) fn with_expected_peer_process_id(mut self, process_id: u32) -> Self {
        self.expected_peer_process_id = Some(process_id);
        self
    }

    fn auth_mode(&self) -> AuthMode {
        match self.shared_secret {
            Some(_) => AuthMode::SharedSecretHmacSha256,
            None => AuthMode::None,
        }
    }

    fn secret(&self) -> Result<Option<&SharedSecret>> {
        match &self.shared_secret {
            Some(Ok(secret)) => Ok(Some(secret)),
            Some(Err(error)) => Err(error.to_error()),
            None => Ok(None),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BootstrapOptions {
    #[cfg(feature = "rustcrypto-auth")]
    channel_id: ChannelId,
    access: Access,
    auth: AuthOptions,
}

impl BootstrapOptions {
    pub(crate) fn new(channel_id: ChannelId, access: Access, auth: AuthOptions) -> Self {
        #[cfg(not(feature = "rustcrypto-auth"))]
        let _ = channel_id;
        Self {
            #[cfg(feature = "rustcrypto-auth")]
            channel_id,
            access,
            auth,
        }
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum AuthMode {
    None = 0,
    SharedSecretHmacSha256 = 1,
}

impl TryFrom<u8> for AuthMode {
    type Error = LavaFlowError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::SharedSecretHmacSha256),
            _ => Err(channel_protocol_error(
                "decode_auth_mode",
                "unknown authentication mode",
            )),
        }
    }
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
    access: Access,
    auth_mode: AuthMode,
}

impl ConnectionHeader {
    fn from_bootstrap(encoding: MetadataEncoding, bootstrap: &BootstrapOptions) -> Self {
        Self {
            encoding,
            access: bootstrap.access,
            auth_mode: bootstrap.auth.auth_mode(),
        }
    }

    fn write_to(self, transport: &mut platform::TransportSender) -> Result<()> {
        transport.write_all(&[
            ProtocolTag::ConnectionHeader as u8,
            LOCAL_PROTOCOL_VERSION,
            self.encoding as u8,
            self.access as u8,
            self.auth_mode as u8,
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

        let mut header = [0_u8; 4];
        transport.read_exact(&mut header)?;
        if header[0] != LOCAL_PROTOCOL_VERSION {
            return Err(channel_protocol_error(
                "read_connection_header",
                "unsupported local protocol version",
            ));
        }

        let encoding = MetadataEncoding::try_from(header[1])?;
        let access = Access::try_from(header[2])?;
        let auth_mode = AuthMode::try_from(header[3])?;
        Ok(Self {
            encoding,
            access,
            auth_mode,
        })
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ProtocolTag {
    ConnectionHeader = 1,
    MessageEnvelope = 2,
    ImportOk = 3,
    ImportFailed = 4,
    ConnectionOk = 5,
    AuthChallenge = 6,
    AuthResponse = 7,
    AuthOk = 8,
    AuthFailed = 9,
}

impl TryFrom<u8> for ProtocolTag {
    type Error = LavaFlowError;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::ConnectionHeader),
            2 => Ok(Self::MessageEnvelope),
            3 => Ok(Self::ImportOk),
            4 => Ok(Self::ImportFailed),
            5 => Ok(Self::ConnectionOk),
            6 => Ok(Self::AuthChallenge),
            7 => Ok(Self::AuthResponse),
            8 => Ok(Self::AuthOk),
            9 => Ok(Self::AuthFailed),
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
        match Self::try_from(tag[0]) {
            Ok(tag) => Ok(tag),
            Err(_) => Err(channel_protocol_error(operation, "unknown protocol tag")),
        }
    }

    fn read_from_receiver(
        transport: &mut platform::TransportReceiver,
        operation: &'static str,
    ) -> Result<Self> {
        let mut tag = [0_u8; 1];
        transport.read_exact(&mut tag)?;
        match Self::try_from(tag[0]) {
            Ok(tag) => Ok(tag),
            Err(_) => Err(channel_protocol_error(operation, "unknown protocol tag")),
        }
    }
}

#[cfg(feature = "rustcrypto-auth")]
#[repr(u8)]
#[derive(Copy, Clone)]
enum EndpointRole {
    Sender = 1,
    Receiver = 2,
}

fn validate_expected_peer_process_id(expected: Option<u32>, actual: u32) -> Result<()> {
    if let Some(expected) = expected
        && actual != expected
    {
        return Err(authentication_failed(
            "peer process id did not match expected value",
        ));
    }
    Ok(())
}

fn authenticate_sender(
    transport: &mut platform::TransportSender,
    header: ConnectionHeader,
    bootstrap: &BootstrapOptions,
) -> Result<()> {
    validate_expected_peer_process_id(
        bootstrap.auth.expected_peer_process_id,
        transport.peer_process_id()?,
    )?;

    match bootstrap.auth.secret()? {
        None => {
            let tag = ProtocolTag::read_from_sender(transport, "read_connection_ok")?;
            match tag {
                ProtocolTag::ConnectionOk => Ok(()),
                ProtocolTag::AuthFailed => Err(authentication_failed("peer rejected bootstrap")),
                _ => Err(channel_protocol_error(
                    "read_connection_ok",
                    "unexpected protocol tag",
                )),
            }
        }
        Some(secret) => {
            #[cfg(feature = "rustcrypto-auth")]
            {
                authenticate_sender_with_secret(transport, header, bootstrap, secret)
            }
            #[cfg(not(feature = "rustcrypto-auth"))]
            {
                let _ = (header, bootstrap, secret);
                Err(auth::unsupported_auth_error())
            }
        }
    }
}

#[cfg(feature = "rustcrypto-auth")]
fn authenticate_sender_with_secret(
    transport: &mut platform::TransportSender,
    header: ConnectionHeader,
    bootstrap: &BootstrapOptions,
    secret: &SharedSecret,
) -> Result<()> {
    let tag = ProtocolTag::read_from_sender(transport, "read_auth_challenge")?;
    match tag {
        ProtocolTag::AuthChallenge => {}
        ProtocolTag::AuthFailed => return Err(authentication_failed("peer rejected bootstrap")),
        _ => {
            return Err(channel_protocol_error(
                "read_auth_challenge",
                "unexpected protocol tag",
            ));
        }
    }

    let mut receiver_nonce = [0_u8; auth::NONCE_SIZE];
    transport.read_exact(&mut receiver_nonce)?;
    let sender_nonce = auth::random_nonce()?;
    let sender_mac = auth::auth_tag(
        secret,
        &auth_transcript(
            bootstrap,
            EndpointRole::Sender,
            header,
            &sender_nonce,
            &receiver_nonce,
        ),
    )?;

    transport.write_all(&[ProtocolTag::AuthResponse as u8])?;
    transport.write_all(&sender_nonce)?;
    transport.write_all(&sender_mac)?;
    transport.flush()?;

    let tag = ProtocolTag::read_from_sender(transport, "read_auth_ok")?;
    match tag {
        ProtocolTag::AuthOk => {}
        ProtocolTag::AuthFailed => return Err(authentication_failed("peer rejected bootstrap")),
        _ => {
            return Err(channel_protocol_error(
                "read_auth_ok",
                "unexpected protocol tag",
            ));
        }
    }

    let mut receiver_mac = [0_u8; auth::TAG_SIZE];
    transport.read_exact(&mut receiver_mac)?;
    auth::verify_auth_tag(
        secret,
        &auth_transcript(
            bootstrap,
            EndpointRole::Receiver,
            header,
            &sender_nonce,
            &receiver_nonce,
        ),
        &receiver_mac,
    )
}

fn authenticate_receiver(
    transport: &mut platform::TransportReceiver,
    header: ConnectionHeader,
    bootstrap: &BootstrapOptions,
) -> Result<()> {
    let result = authenticate_receiver_inner(transport, header, bootstrap);
    if result.is_err() {
        let _ = transport.write_all(&[ProtocolTag::AuthFailed as u8]);
        let _ = transport.flush();
    }
    result
}

fn authenticate_receiver_inner(
    transport: &mut platform::TransportReceiver,
    header: ConnectionHeader,
    bootstrap: &BootstrapOptions,
) -> Result<()> {
    validate_expected_peer_process_id(
        bootstrap.auth.expected_peer_process_id,
        transport.peer_process_id()?,
    )?;
    let secret = bootstrap.auth.secret()?;
    if header.access != bootstrap.access {
        return Err(authentication_failed("local access policies do not match"));
    }
    if header.auth_mode != bootstrap.auth.auth_mode() {
        return Err(authentication_failed("authentication modes do not match"));
    }

    match secret {
        None => {
            transport.write_all(&[ProtocolTag::ConnectionOk as u8])?;
            transport.flush()
        }
        Some(secret) => {
            #[cfg(feature = "rustcrypto-auth")]
            {
                authenticate_receiver_with_secret(transport, header, bootstrap, secret)
            }
            #[cfg(not(feature = "rustcrypto-auth"))]
            {
                let _ = (header, bootstrap, secret);
                Err(auth::unsupported_auth_error())
            }
        }
    }
}

#[cfg(feature = "rustcrypto-auth")]
fn authenticate_receiver_with_secret(
    transport: &mut platform::TransportReceiver,
    header: ConnectionHeader,
    bootstrap: &BootstrapOptions,
    secret: &SharedSecret,
) -> Result<()> {
    let receiver_nonce = auth::random_nonce()?;
    transport.write_all(&[ProtocolTag::AuthChallenge as u8])?;
    transport.write_all(&receiver_nonce)?;
    transport.flush()?;

    let tag = ProtocolTag::read_from_receiver(transport, "read_auth_response")?;
    if tag != ProtocolTag::AuthResponse {
        return Err(channel_protocol_error(
            "read_auth_response",
            "unexpected protocol tag",
        ));
    }
    let mut sender_nonce = [0_u8; auth::NONCE_SIZE];
    let mut sender_mac = [0_u8; auth::TAG_SIZE];
    transport.read_exact(&mut sender_nonce)?;
    transport.read_exact(&mut sender_mac)?;

    auth::verify_auth_tag(
        secret,
        &auth_transcript(
            bootstrap,
            EndpointRole::Sender,
            header,
            &sender_nonce,
            &receiver_nonce,
        ),
        &sender_mac,
    )?;
    let receiver_mac = auth::auth_tag(
        secret,
        &auth_transcript(
            bootstrap,
            EndpointRole::Receiver,
            header,
            &sender_nonce,
            &receiver_nonce,
        ),
    )?;
    transport.write_all(&[ProtocolTag::AuthOk as u8])?;
    transport.write_all(&receiver_mac)?;
    transport.flush()
}

#[cfg(feature = "rustcrypto-auth")]
fn auth_transcript(
    bootstrap: &BootstrapOptions,
    endpoint_role: EndpointRole,
    header: ConnectionHeader,
    sender_nonce: &[u8; auth::NONCE_SIZE],
    receiver_nonce: &[u8; auth::NONCE_SIZE],
) -> Vec<u8> {
    let mut transcript = Vec::new();
    transcript.extend_from_slice(auth::TRANSCRIPT_DOMAIN);
    transcript.push(LOCAL_PROTOCOL_VERSION);
    transcript.push(header.encoding as u8);
    transcript.push(header.access as u8);
    transcript.push(header.auth_mode as u8);
    transcript.push(bootstrap.access as u8);
    transcript.push(endpoint_role as u8);
    transcript.extend_from_slice(bootstrap.channel_id.as_str().as_bytes());
    transcript.extend_from_slice(sender_nonce);
    transcript.extend_from_slice(receiver_nonce);
    transcript
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

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum FrameHeader {
    Cpu { buffer_size: usize },
    Gpu { buffer_size: usize, device_id: u32 },
}

impl FrameHeader {
    fn from_frame(frame: &Frame) -> Self {
        match frame {
            Frame::Cpu(buffer) => Self::Cpu {
                buffer_size: buffer.size(),
            },
            Frame::Gpu(buffer) => Self::Gpu {
                buffer_size: buffer.size(),
                device_id: buffer.device_id(),
            },
        }
    }

    fn kind(self) -> FrameKind {
        match self {
            Self::Cpu { .. } => FrameKind::Cpu,
            Self::Gpu { .. } => FrameKind::Gpu,
        }
    }

    fn buffer_size(self) -> usize {
        match self {
            Self::Cpu { buffer_size } | Self::Gpu { buffer_size, .. } => buffer_size,
        }
    }

    fn write_to(self, transport: &mut platform::TransportSender) -> Result<()> {
        transport.write_all(&[self.kind() as u8])?;
        transport.write_all(&(self.buffer_size() as u64).to_le_bytes())?;
        if let Self::Gpu { device_id, .. } = self {
            transport.write_all(&device_id.to_le_bytes())?;
        }
        Ok(())
    }

    fn read_from(
        transport: &mut platform::TransportReceiver,
        limits: ProtocolLimits,
    ) -> Result<Self> {
        let mut kind = [0_u8; 1];
        transport.read_exact(&mut kind)?;
        let kind = FrameKind::try_from(kind[0])?;

        let mut buffer_size_bytes = [0_u8; 8];
        transport.read_exact(&mut buffer_size_bytes)?;
        let buffer_size = match usize::try_from(u64::from_le_bytes(buffer_size_bytes)) {
            Ok(buffer_size) => buffer_size,
            Err(_) => {
                return Err(channel_protocol_error(
                    "read_message_envelope",
                    "buffer size overflow",
                ));
            }
        };
        limits.validate_inbound_payload_size(buffer_size)?;

        match kind {
            FrameKind::Cpu => Ok(Self::Cpu { buffer_size }),
            FrameKind::Gpu => {
                let mut device_id_bytes = [0_u8; 4];
                transport.read_exact(&mut device_id_bytes)?;
                Ok(Self::Gpu {
                    buffer_size,
                    device_id: u32::from_le_bytes(device_id_bytes),
                })
            }
        }
    }
}

/// Sender half of the local IPC transport.
///
/// This transport uses an OS-native local IPC primitive to transfer the control envelope while the
/// payload bytes stay in shared memory and are referenced through an exported CPU or GPU handle.
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
        bootstrap: BootstrapOptions,
    ) -> Result<SenderListener> {
        Ok(SenderListener {
            encoding,
            listener: platform::TransportListener::bind(address, bootstrap.access)?,
            limits,
            bootstrap,
        })
    }

    /// Sends a payload frame with typed metadata through local IPC.
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
        let result = match envelope.write_to(&mut self.transport) {
            Ok(()) => self.recv_import_ack(),
            Err(error) => Err(error),
        };
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

    /// Sends a payload frame with dynamic metadata through local IPC.
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
        let result = match envelope.write_to(&mut self.transport) {
            Ok(()) => self.recv_import_ack(),
            Err(error) => Err(error),
        };
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
    bootstrap: BootstrapOptions,
) -> Result<SenderListener> {
    let address = EndpointAddress::from_channel(channel_id, bootstrap.access);
    Sender::listen(encoding, &address, limits, bootstrap)
}

#[derive(Debug)]
pub(crate) struct SenderListener {
    encoding: MetadataEncoding,
    listener: platform::TransportListener,
    limits: ProtocolLimits,
    bootstrap: BootstrapOptions,
}

impl SenderListener {
    pub(crate) fn accept(self) -> Result<Sender> {
        let mut transport = self.listener.accept()?;
        let header = ConnectionHeader::from_bootstrap(self.encoding, &self.bootstrap);
        header.write_to(&mut transport)?;
        authenticate_sender(&mut transport, header, &self.bootstrap)?;
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
                    let header = ConnectionHeader::from_bootstrap(self.encoding, &self.bootstrap);
                    header.write_to(&mut transport)?;
                    authenticate_sender(&mut transport, header, &self.bootstrap)?;
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

/// Receiver half of the local IPC transport.
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

    pub(crate) fn connect(
        address: &EndpointAddress,
        limits: ProtocolLimits,
        bootstrap: BootstrapOptions,
    ) -> Result<Self> {
        let mut transport = platform::TransportReceiver::connect(address)?;
        let header = ConnectionHeader::read_from(&mut transport)?;
        authenticate_receiver(&mut transport, header, &bootstrap)?;
        Ok(Self::new(header.encoding, transport, limits))
    }

    /// Receives a payload frame with typed metadata through local IPC.
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

    /// Receives a payload frame with dynamic metadata through local IPC.
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
    bootstrap: BootstrapOptions,
) -> Result<Receiver> {
    let address = EndpointAddress::from_channel(channel_id, bootstrap.access);
    Receiver::connect(&address, limits, bootstrap)
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
    header: FrameHeader,
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
        let (header, handle) = Self::export_frame(frame)?;
        limits.validate_outbound_payload_size(header.buffer_size())?;
        limits.validate_outbound_metadata_len(metadata.len())?;
        Ok(Self {
            header,
            handle,
            metadata,
        })
    }

    fn write_to(self, transport: &mut platform::TransportSender) -> Result<()> {
        let Self {
            header,
            handle,
            metadata,
        } = self;
        let metadata_len = match u32::try_from(metadata.len()) {
            Ok(metadata_len) => metadata_len,
            Err(_) => {
                return Err(channel_protocol_error(
                    "write_message_envelope",
                    "metadata length overflow",
                ));
            }
        };

        transport.write_all(&[ProtocolTag::MessageEnvelope as u8])?;
        header.write_to(transport)?;
        transport.send_handle(handle)?;
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

        let header = FrameHeader::read_from(transport, limits)?;

        // Local OS handles/fds do not carry the memory backend type. The protocol header is the
        // typed part of the transfer, and the platform layer wraps the raw handle accordingly.
        let handle = transport.recv_handle(header.kind())?;

        let mut metadata_len_bytes = [0_u8; 4];
        transport.read_exact(&mut metadata_len_bytes)?;
        let metadata_len = u32::from_le_bytes(metadata_len_bytes) as usize;
        limits.validate_inbound_metadata_len(metadata_len)?;
        let mut metadata = vec![0_u8; metadata_len];
        transport.read_exact(&mut metadata)?;

        Ok(Self {
            header,
            handle,
            metadata,
        })
    }

    fn decode_metadata<M: Metadata>(&self, encoding: MetadataEncoding) -> Result<M> {
        match encoding {
            MetadataEncoding::Json => {
                serde_json::from_slice(&self.metadata).map_err(metadata_deserialize_error)
            }
            MetadataEncoding::Cbor => {
                Err(LavaFlowError::UnsupportedMetadataEncoding { encoding: "cbor" })
            }
        }
    }

    fn try_into_frame(&self) -> Result<Frame> {
        match self.header {
            FrameHeader::Cpu { buffer_size } => {
                let buffer =
                    cpu::MemoryBuffer::from_shared_handle(buffer_size, self.handle.try_clone()?)?;
                Ok(Frame::Cpu(buffer))
            }
            FrameHeader::Gpu {
                buffer_size,
                device_id,
            } => {
                let buffer = gpu::MemoryBuffer::from_shared_handle(
                    device_id,
                    buffer_size,
                    self.handle.try_clone()?,
                )?;
                Ok(Frame::Gpu(buffer))
            }
        }
    }

    fn encode_metadata<M: Metadata>(encoding: MetadataEncoding, metadata: &M) -> Result<Vec<u8>> {
        match encoding {
            MetadataEncoding::Json => {
                serde_json::to_vec(metadata).map_err(metadata_serialize_error)
            }
            MetadataEncoding::Cbor => {
                Err(LavaFlowError::UnsupportedMetadataEncoding { encoding: "cbor" })
            }
        }
    }

    fn export_frame(frame: Frame) -> Result<(FrameHeader, InterprocessMemoryHandle)> {
        let header = FrameHeader::from_frame(&frame);
        let handle = match frame {
            Frame::Cpu(buffer) => buffer.shared_handle()?,
            Frame::Gpu(buffer) => buffer.shared_handle()?,
        };
        Ok((header, handle))
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

fn authentication_failed(reason: &'static str) -> LavaFlowError {
    LavaFlowError::ChannelAuthenticationFailed { reason }
}

fn metadata_deserialize_error(source: serde_json::Error) -> LavaFlowError {
    LavaFlowError::ChannelMetadataCodec {
        operation: "deserialize_metadata",
        source,
    }
}

fn metadata_serialize_error(source: serde_json::Error) -> LavaFlowError {
    LavaFlowError::ChannelMetadataCodec {
        operation: "serialize_metadata",
        source,
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
            pub(in crate::channel::local) width: u32,
            pub(in crate::channel::local) height: u32,
        }

        #[derive(Clone, Debug, Deserialize)]
        pub(in crate::channel::local) struct FailingMeta;

        impl Serialize for FailingMeta {
            fn serialize<S>(&self, _serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                Err(serde::ser::Error::custom("failing metadata serializer"))
            }
        }

        pub(in crate::channel::local) const BUFFER_SIZE: usize = 64;
        pub(in crate::channel::local) const TEST_BYTE_OFFSET: usize = 7;
        pub(in crate::channel::local) const TEST_BYTE_VALUE: u8 = 0x5a;

        pub(in crate::channel::local) fn test_address() -> EndpointAddress {
            platform::tests::support::test_address()
        }

        pub(in crate::channel::local) fn test_bootstrap_options() -> BootstrapOptions {
            BootstrapOptions::new(
                ChannelId::new("local-test-channel").expect("channel id"),
                Access::CurrentSession,
                AuthOptions::default(),
            )
        }

        pub(in crate::channel::local) fn test_pair(
            encoding: MetadataEncoding,
        ) -> Result<(Sender, Receiver)> {
            let address = test_address();
            let bootstrap = test_bootstrap_options();
            let listener = Sender::listen(
                encoding,
                &address,
                ProtocolLimits::default(),
                bootstrap.clone(),
            )?;
            let receiver_address = address.clone();
            let receiver_thread = thread::spawn(move || {
                Receiver::connect(&receiver_address, ProtocolLimits::default(), bootstrap)
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
        BUFFER_SIZE, FailingMeta, TEST_BYTE_OFFSET, TEST_BYTE_VALUE, TestMeta, test_allocator,
        test_pair, test_transport_pair,
    };

    const SMALL_PAYLOAD_LIMIT: usize = BUFFER_SIZE - 1;
    const SMALL_METADATA_LIMIT: usize = 8;
    #[cfg(feature = "rustcrypto-auth")]
    const TEST_SHARED_SECRET: &[u8] = b"local auth coverage shared secret";

    fn test_auth_bootstrap(secret: Option<&[u8]>) -> BootstrapOptions {
        let auth = match secret {
            Some(secret) => AuthOptions::default().with_shared_secret(secret.to_vec()),
            None => AuthOptions::default(),
        };
        BootstrapOptions::new(
            ChannelId::new("local-auth-test-channel").expect("channel id"),
            Access::CurrentSession,
            auth,
        )
    }

    #[test]
    fn cpu_local_ipc_round_trips_typed_metadata_and_shared_payload() {
        let (mut sender, mut receiver) =
            test_pair(MetadataEncoding::Json).expect("create cpu local ipc pair");
        let mut buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        buffer.as_mut_slice()[TEST_BYTE_OFFSET] = TEST_BYTE_VALUE;

        let metadata = TestMeta {
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
        let metadata = MessageMeta { values };
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
    fn gpu_local_ipc_round_trips_typed_metadata_and_external_handle() {
        let (mut sender, mut receiver) =
            test_pair(MetadataEncoding::Json).expect("create local ipc pair");
        let gpu_allocator = match crate::memory::gpu::Allocator::new() {
            Ok(allocator) => allocator,
            Err(_) => return,
        };
        let buffer = gpu_allocator
            .allocate(BUFFER_SIZE)
            .expect("allocate gpu payload");
        let metadata = TestMeta {
            width: 16,
            height: 16,
        };

        // send() waits for the receiver-side import ACK, so recv() must run concurrently.
        let recv_thread = thread::spawn(move || {
            let (frame, received) = receiver.recv::<TestMeta>().expect("receive gpu frame");
            let Frame::Gpu(imported) = frame else {
                panic!("expected gpu frame");
            };
            (received, imported.size(), imported.device_id())
        });

        sender
            .send(Frame::Gpu(buffer), &metadata)
            .expect("send gpu frame");

        let (received, imported_size, imported_device_id) =
            recv_thread.join().expect("receiver thread must not panic");
        assert_eq!(received, metadata);
        assert_eq!(imported_size, BUFFER_SIZE);
        assert_eq!(imported_device_id, gpu_allocator.device_id());
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
    fn auth_options_reject_empty_shared_secret() {
        let auth = AuthOptions::default().with_shared_secret(Vec::new());
        let err = auth
            .secret()
            .expect_err("empty shared secret must be rejected");
        assert!(matches!(
            err,
            LavaFlowError::ChannelAuthenticationFailed { .. }
        ));
    }

    #[test]
    fn shared_secret_debug_redacts_secret_bytes() {
        let secret = SharedSecret::from_test_secret(b"local auth coverage shared secret".to_vec());
        let debug = format!("{secret:?}");

        assert_eq!(debug, "SharedSecret(\"<redacted>\")");
        assert!(!debug.contains("coverage"));
    }

    #[test]
    fn auth_options_can_return_constructed_secret() {
        let auth = AuthOptions {
            shared_secret: Some(Ok(SharedSecret::from_test_secret(
                b"constructed test secret".to_vec(),
            ))),
            expected_peer_process_id: None,
        };

        assert!(auth.secret().expect("secret must decode").is_some());
    }

    #[test]
    fn protocol_tag_decodes_auth_control_tags() {
        assert_eq!(
            ProtocolTag::try_from(ProtocolTag::AuthChallenge as u8).expect("auth challenge tag"),
            ProtocolTag::AuthChallenge,
        );
        assert_eq!(
            ProtocolTag::try_from(ProtocolTag::AuthResponse as u8).expect("auth response tag"),
            ProtocolTag::AuthResponse,
        );
        assert_eq!(
            ProtocolTag::try_from(ProtocolTag::AuthOk as u8).expect("auth ok tag"),
            ProtocolTag::AuthOk,
        );
        assert_eq!(
            ProtocolTag::try_from(ProtocolTag::AuthFailed as u8).expect("auth failed tag"),
            ProtocolTag::AuthFailed,
        );
    }

    #[test]
    fn expected_peer_process_id_rejects_mismatch() {
        let err = validate_expected_peer_process_id(Some(11), 12)
            .expect_err("mismatched peer pid must fail");
        assert!(matches!(
            err,
            LavaFlowError::ChannelAuthenticationFailed { .. }
        ));
    }

    #[cfg(not(feature = "rustcrypto-auth"))]
    #[test]
    fn rustcrypto_auth_helpers_report_unsupported_when_feature_is_disabled() {
        let secret = SharedSecret::from_test_secret(Vec::new());

        let nonce_err = auth::random_nonce()
            .expect_err("nonce generation must require rustcrypto-auth feature");
        let tag_err = auth::auth_tag(&secret, b"transcript")
            .expect_err("auth tag must require rustcrypto-auth feature");
        let verify_err = auth::verify_auth_tag(&secret, b"transcript", &[0_u8; auth::TAG_SIZE])
            .expect_err("auth tag verification must require rustcrypto-auth feature");

        for err in [nonce_err, tag_err, verify_err] {
            assert!(matches!(
                err,
                LavaFlowError::UnsupportedChannelAuthentication {
                    mechanism: "shared-secret-hmac-sha256"
                }
            ));
        }
    }

    #[test]
    fn receiver_rejects_unsupported_protocol_version() {
        let address = support::test_address();
        let listener = platform::TransportListener::bind(&address, Access::CurrentSession)
            .expect("bind transport");
        let receiver_address = address.clone();
        let bootstrap = support::test_bootstrap_options();
        let receiver_thread = thread::spawn(move || {
            Receiver::connect(&receiver_address, ProtocolLimits::default(), bootstrap)
        });

        let mut sender_transport = listener.accept().expect("accept transport");
        sender_transport
            .write_all(&[
                ProtocolTag::ConnectionHeader as u8,
                LOCAL_PROTOCOL_VERSION + 1,
                MetadataEncoding::Json as u8,
                Access::CurrentSession as u8,
                AuthMode::None as u8,
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
    fn receiver_rejects_unknown_auth_mode_in_connection_header() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[
                ProtocolTag::ConnectionHeader as u8,
                LOCAL_PROTOCOL_VERSION,
                MetadataEncoding::Json as u8,
                Access::CurrentSession as u8,
                99,
            ])
            .expect("write invalid connection header");
        sender_transport
            .flush()
            .expect("flush invalid connection header");

        let err = ConnectionHeader::read_from(&mut receiver_transport)
            .expect_err("receiver must reject unknown authentication mode");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "decode_auth_mode",
                ..
            }
        ));
    }

    #[test]
    fn sender_rejects_unexpected_connection_ack_tag() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        receiver_transport
            .write_all(&[ProtocolTag::MessageEnvelope as u8])
            .expect("write unexpected connection ack tag");
        receiver_transport
            .flush()
            .expect("flush unexpected connection ack tag");
        let bootstrap = test_auth_bootstrap(None);
        let header = ConnectionHeader::from_bootstrap(MetadataEncoding::Json, &bootstrap);

        let err = authenticate_sender(&mut sender_transport, header, &bootstrap)
            .expect_err("sender must reject unexpected connection ack tag");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_connection_ok",
                ..
            }
        ));
    }

    #[test]
    fn sender_reports_auth_failed_connection_ack() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        receiver_transport
            .write_all(&[ProtocolTag::AuthFailed as u8])
            .expect("write auth failed tag");
        receiver_transport.flush().expect("flush auth failed tag");
        let bootstrap = test_auth_bootstrap(None);
        let header = ConnectionHeader::from_bootstrap(MetadataEncoding::Json, &bootstrap);

        let err = authenticate_sender(&mut sender_transport, header, &bootstrap)
            .expect_err("sender must report auth failure from receiver");
        assert!(matches!(
            err,
            LavaFlowError::ChannelAuthenticationFailed { .. }
        ));
    }

    #[test]
    fn sender_rejects_unknown_protocol_tag_when_reading_control_reply() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        receiver_transport
            .write_all(&[99])
            .expect("write unknown protocol tag");
        receiver_transport
            .flush()
            .expect("flush unknown protocol tag");

        let err = ProtocolTag::read_from_sender(&mut sender_transport, "read_test_control")
            .expect_err("sender must reject unknown protocol tag");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_test_control",
                ..
            }
        ));
    }

    #[test]
    fn receiver_rejects_access_policy_mismatch() {
        let (_sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let bootstrap = test_auth_bootstrap(None);
        let header = ConnectionHeader {
            encoding: MetadataEncoding::Json,
            access: Access::AuthenticatedUsers,
            auth_mode: AuthMode::None,
        };

        let err = authenticate_receiver(&mut receiver_transport, header, &bootstrap)
            .expect_err("receiver must reject mismatched local access policy");
        assert!(matches!(
            err,
            LavaFlowError::ChannelAuthenticationFailed { .. }
        ));
    }

    #[test]
    fn receiver_rejects_auth_mode_mismatch() {
        let (_sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let bootstrap = test_auth_bootstrap(None);
        let header = ConnectionHeader {
            encoding: MetadataEncoding::Json,
            access: Access::CurrentSession,
            auth_mode: AuthMode::SharedSecretHmacSha256,
        };

        let err = authenticate_receiver(&mut receiver_transport, header, &bootstrap)
            .expect_err("receiver must reject mismatched auth mode");
        assert!(matches!(
            err,
            LavaFlowError::ChannelAuthenticationFailed { .. }
        ));
    }

    #[cfg(feature = "rustcrypto-auth")]
    #[test]
    fn sender_rejects_unexpected_auth_challenge_tag() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        receiver_transport
            .write_all(&[ProtocolTag::ConnectionOk as u8])
            .expect("write unexpected auth challenge tag");
        receiver_transport
            .flush()
            .expect("flush unexpected auth challenge tag");
        let bootstrap = test_auth_bootstrap(Some(TEST_SHARED_SECRET));
        let header = ConnectionHeader::from_bootstrap(MetadataEncoding::Json, &bootstrap);

        let err = authenticate_sender(&mut sender_transport, header, &bootstrap)
            .expect_err("sender must reject unexpected auth challenge tag");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_auth_challenge",
                ..
            }
        ));
    }

    #[cfg(feature = "rustcrypto-auth")]
    #[test]
    fn sender_rejects_unexpected_auth_ok_tag() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        receiver_transport
            .write_all(&[ProtocolTag::AuthChallenge as u8])
            .expect("write auth challenge tag");
        receiver_transport
            .write_all(&[0x5a; auth::NONCE_SIZE])
            .expect("write receiver nonce");
        receiver_transport
            .write_all(&[ProtocolTag::ConnectionOk as u8])
            .expect("write unexpected auth ok tag");
        receiver_transport.flush().expect("flush auth challenge");
        let bootstrap = test_auth_bootstrap(Some(TEST_SHARED_SECRET));
        let header = ConnectionHeader::from_bootstrap(MetadataEncoding::Json, &bootstrap);

        let err = authenticate_sender(&mut sender_transport, header, &bootstrap)
            .expect_err("sender must reject unexpected auth ok tag");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_auth_ok",
                ..
            }
        ));
    }

    #[cfg(feature = "rustcrypto-auth")]
    #[test]
    fn receiver_rejects_unexpected_auth_response_tag() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[ProtocolTag::ConnectionOk as u8])
            .expect("write unexpected auth response tag");
        sender_transport
            .flush()
            .expect("flush unexpected auth response tag");
        let bootstrap = test_auth_bootstrap(Some(TEST_SHARED_SECRET));
        let header = ConnectionHeader::from_bootstrap(MetadataEncoding::Json, &bootstrap);

        let err = authenticate_receiver(&mut receiver_transport, header, &bootstrap)
            .expect_err("receiver must reject unexpected auth response tag");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "read_auth_response",
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
                Access::CurrentSession as u8,
                AuthMode::None as u8,
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
    fn receiver_rejects_unknown_local_access_in_connection_header() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        sender_transport
            .write_all(&[
                ProtocolTag::ConnectionHeader as u8,
                LOCAL_PROTOCOL_VERSION,
                MetadataEncoding::Json as u8,
                99,
                AuthMode::None as u8,
            ])
            .expect("write invalid connection header");
        sender_transport
            .flush()
            .expect("flush invalid connection header");

        let err = ConnectionHeader::read_from(&mut receiver_transport)
            .expect_err("receiver must reject unknown local access policy");
        assert!(matches!(
            err,
            LavaFlowError::ChannelTransportOperation {
                operation: "decode_local_access",
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
    fn message_envelope_reports_json_metadata_decode_errors() {
        let buffer = test_allocator()
            .allocate(BUFFER_SIZE)
            .expect("allocate payload");
        let handle = buffer.shared_handle().expect("export shared handle");
        let envelope = MessageEnvelope {
            header: FrameHeader::Cpu {
                buffer_size: BUFFER_SIZE,
            },
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
            header: FrameHeader::Cpu {
                buffer_size: BUFFER_SIZE,
            },
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
            header: FrameHeader::Cpu {
                buffer_size: BUFFER_SIZE,
            },
            handle: typed_handle,
            metadata: typed_bytes,
        };
        let decoded_typed = typed_envelope
            .decode_metadata::<TestMeta>(MetadataEncoding::Json)
            .expect("decode typed metadata");
        assert_eq!(decoded_typed, typed_metadata);

        let dynamic_metadata = MessageMeta {
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
            header: FrameHeader::Cpu {
                buffer_size: BUFFER_SIZE,
            },
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
        assert_eq!(
            read_back.header,
            FrameHeader::Cpu {
                buffer_size: BUFFER_SIZE
            }
        );
        assert!(crate::memory::allocator::tests::support::handle_is_cpu(
            &read_back.handle
        ));
    }

    #[test]
    fn frame_header_gpu_round_trips_device_id() {
        let (mut sender_transport, mut receiver_transport) =
            test_transport_pair().expect("create transport pair");
        let header = FrameHeader::Gpu {
            buffer_size: BUFFER_SIZE,
            device_id: 7,
        };

        header
            .write_to(&mut sender_transport)
            .expect("write gpu frame header");
        sender_transport.flush().expect("flush gpu frame header");
        let read_back = FrameHeader::read_from(&mut receiver_transport, ProtocolLimits::default())
            .expect("read gpu frame header");

        assert_eq!(read_back, header);
    }

    #[test]
    fn message_envelope_write_rejects_payloads_above_configured_limit() {
        let limits = ProtocolLimits::with_max_sizes(
            SMALL_PAYLOAD_LIMIT,
            DEFAULT_MAX_LOCAL_CHANNEL_METADATA_SIZE,
        );
        let metadata = TestMeta {
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
            .send_handle(handle)
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
    fn message_envelope_imports_gpu_handle_frame() {
        let gpu_allocator = match crate::memory::gpu::Allocator::new() {
            Ok(allocator) => allocator,
            Err(_) => return,
        };
        let buffer = gpu_allocator
            .allocate(BUFFER_SIZE)
            .expect("allocate gpu payload");
        let handle = buffer.shared_handle().expect("export gpu shared handle");
        let envelope = MessageEnvelope {
            header: FrameHeader::Gpu {
                buffer_size: BUFFER_SIZE,
                device_id: gpu_allocator.device_id(),
            },
            handle,
            metadata: Vec::new(),
        };

        let frame = envelope
            .try_into_frame()
            .expect("gpu frame import must succeed");
        let Frame::Gpu(imported) = frame else {
            panic!("expected gpu frame");
        };
        assert_eq!(imported.size(), BUFFER_SIZE);
        assert_eq!(imported.device_id(), gpu_allocator.device_id());
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
        let metadata = serde_json::to_vec(&TestMeta {
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
            .write_all(&0x1234_u64.to_le_bytes())
            .expect("write invalid non-null handle value");
        sender_transport
            .write_all(&(metadata.len() as u32).to_le_bytes())
            .expect("write metadata length");
        sender_transport
            .write_all(&metadata)
            .expect("write metadata bytes");
        sender_transport.flush().expect("flush message");

        let err = receiver
            .recv::<TestMeta>()
            .expect_err("receiver must fail when transferred cpu handle is invalid");
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
        let metadata = serde_json::to_vec(&MessageMeta {
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
            .write_all(&0x1234_u64.to_le_bytes())
            .expect("write invalid non-null handle value");
        sender_transport
            .write_all(&(metadata.len() as u32).to_le_bytes())
            .expect("write metadata length");
        sender_transport
            .write_all(&metadata)
            .expect("write metadata bytes");
        sender_transport.flush().expect("flush message");

        let err = receiver
            .recv_map()
            .expect_err("receiver must fail when transferred cpu handle is invalid");
        assert!(matches!(err, LavaFlowError::SharedMemoryOperation { .. }));

        let ack = ProtocolTag::read_from_sender(&mut sender_transport, "read_import_ack")
            .expect("read import-failed ack");
        assert_eq!(ack, ProtocolTag::ImportFailed);
    }
}
