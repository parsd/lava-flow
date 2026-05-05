#[path = "local_ipc/gpu.rs"]
mod gpu_support;
#[path = "local_ipc/harness.rs"]
mod harness;

use gpu_support::GpuInterop;
use harness::{BUILD_TIMEOUT, ChildConfig, ReadyFile, run_interprocess_case};
use lava_flow::channel::{Builder, Frame, MetadataEncoding, Receiver};
use lava_flow::memory::{cpu, gpu};
use lava_flow::types::ChannelId;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::io;

#[derive(Clone, Copy)]
pub(crate) enum IpcCase {
    Cpu,
    Gpu,
    Mixed,
}

impl IpcCase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum TransferKind {
    Cpu,
    Gpu,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
struct PayloadMeta {
    kind: TransferKind,
    sequence: usize,
    size: usize,
    seed: u8,
}

impl PayloadMeta {
    fn new(kind: TransferKind, sequence: usize, size: usize, seed: u8) -> Self {
        Self {
            kind,
            sequence,
            size,
            seed,
        }
    }
}

#[test]
fn cpu_local_ipc_transfers_buffer_contents_between_processes() -> Result<(), Box<dyn Error>> {
    run_interprocess_case(IpcCase::Cpu, 4096, 0x5A)
}

#[test]
fn gpu_local_ipc_transfers_buffer_contents_between_processes() -> Result<(), Box<dyn Error>> {
    if gpu::Allocator::new().is_err() {
        return Ok(());
    }

    run_interprocess_case(IpcCase::Gpu, 4096, 0xA5)
}

#[test]
fn mixed_local_ipc_repeats_cpu_and_gpu_transfers_between_processes() -> Result<(), Box<dyn Error>> {
    if gpu::Allocator::new().is_err() {
        return Ok(());
    }

    run_interprocess_case(IpcCase::Mixed, 4096, 0x3C)
}

#[test]
fn local_ipc_child_entry() -> Result<(), Box<dyn Error>> {
    let Some(config) = ChildConfig::new()? else {
        return Ok(());
    };

    match (config.role.as_str(), config.case.as_str()) {
        ("sender", "cpu") => run_cpu_sender(config.channel_id, config.size, config.seed),
        ("receiver", "cpu") => run_cpu_receiver(config.channel_id, config.size, config.seed),
        ("sender", "gpu") => run_gpu_sender(config.channel_id, config.size, config.seed),
        ("receiver", "gpu") => run_gpu_receiver(config.channel_id, config.size, config.seed),
        ("sender", "mixed") => run_mixed_sender(config.channel_id, config.size, config.seed),
        ("receiver", "mixed") => run_mixed_receiver(config.channel_id, config.size, config.seed),
        _ => Err(unknown_child_config_error(config).into()),
    }
}

fn unknown_child_config_error(config: ChildConfig) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "unknown local IPC child role/case: {}/{}",
            config.role, config.case
        ),
    )
}

fn run_cpu_sender(channel_id: ChannelId, size: usize, seed: u8) -> Result<(), Box<dyn Error>> {
    let allocator = cpu::Allocator::new();
    let metadata = single_transfer(TransferKind::Cpu, size, seed);
    let buffer = build_cpu_buffer(&allocator, metadata)?;
    ReadyFile::signal()?;
    let mut sender = Builder::local_sender(channel_id)?
        .with_metadata_encoding(MetadataEncoding::Json)
        .build_with_timeout(BUILD_TIMEOUT)?;
    sender.send(buffer, &metadata)?;
    Ok(())
}

fn run_cpu_receiver(channel_id: ChannelId, size: usize, seed: u8) -> Result<(), Box<dyn Error>> {
    let mut receiver = Builder::local_receiver(channel_id)?.build_with_timeout(BUILD_TIMEOUT)?;
    recv_transfer(
        &mut receiver,
        single_transfer(TransferKind::Cpu, size, seed),
    )
}

fn run_gpu_sender(channel_id: ChannelId, size: usize, seed: u8) -> Result<(), Box<dyn Error>> {
    let allocator = gpu::Allocator::new()?;
    let interop = GpuInterop::new(allocator.device_id())?;
    let metadata = single_transfer(TransferKind::Gpu, size, seed);
    let buffer = build_gpu_buffer(&allocator, &interop, metadata)?;
    ReadyFile::signal()?;
    let mut sender = Builder::local_sender(channel_id)?
        .with_metadata_encoding(MetadataEncoding::Json)
        .build_with_timeout(BUILD_TIMEOUT)?;
    sender.send(buffer, &metadata)?;
    Ok(())
}

fn run_gpu_receiver(channel_id: ChannelId, size: usize, seed: u8) -> Result<(), Box<dyn Error>> {
    let mut receiver = Builder::local_receiver(channel_id)?.build_with_timeout(BUILD_TIMEOUT)?;
    recv_transfer(
        &mut receiver,
        single_transfer(TransferKind::Gpu, size, seed),
    )
}

fn run_mixed_sender(channel_id: ChannelId, size: usize, seed: u8) -> Result<(), Box<dyn Error>> {
    let cpu_allocator = cpu::Allocator::new();
    let gpu_allocator = gpu::Allocator::new()?;
    let gpu_interop = GpuInterop::new(gpu_allocator.device_id())?;
    let transfers = mixed_transfers(size, seed);

    ReadyFile::signal()?;
    let mut sender = Builder::local_sender(channel_id)?
        .with_metadata_encoding(MetadataEncoding::Json)
        .build_with_timeout(BUILD_TIMEOUT)?;

    for metadata in transfers {
        match metadata.kind {
            TransferKind::Cpu => {
                let buffer = build_cpu_buffer(&cpu_allocator, metadata)?;
                sender.send(buffer, &metadata)?;
            }
            TransferKind::Gpu => {
                let buffer = build_gpu_buffer(&gpu_allocator, &gpu_interop, metadata)?;
                sender.send(buffer, &metadata)?;
            }
        }
    }
    Ok(())
}

fn run_mixed_receiver(channel_id: ChannelId, size: usize, seed: u8) -> Result<(), Box<dyn Error>> {
    let mut receiver = Builder::local_receiver(channel_id)?.build_with_timeout(BUILD_TIMEOUT)?;

    for metadata in mixed_transfers(size, seed) {
        recv_transfer(&mut receiver, metadata)?;
    }
    Ok(())
}

fn single_transfer(kind: TransferKind, size: usize, seed: u8) -> PayloadMeta {
    PayloadMeta::new(kind, 0, size, seed)
}

fn mixed_transfers(size: usize, seed: u8) -> [PayloadMeta; 4] {
    [
        PayloadMeta::new(TransferKind::Cpu, 0, size, seed),
        PayloadMeta::new(TransferKind::Gpu, 1, size / 2, seed.wrapping_add(1)),
        PayloadMeta::new(TransferKind::Cpu, 2, size / 4, seed.wrapping_add(2)),
        PayloadMeta::new(TransferKind::Gpu, 3, size / 8, seed.wrapping_add(3)),
    ]
}

fn build_cpu_buffer(
    allocator: &cpu::Allocator,
    metadata: PayloadMeta,
) -> Result<cpu::MemoryBuffer, Box<dyn Error>> {
    let mut buffer = allocator.allocate(metadata.size)?;
    fill_pattern(buffer.as_mut_slice(), metadata.seed);
    Ok(buffer)
}

fn build_gpu_buffer(
    allocator: &gpu::Allocator,
    interop: &GpuInterop,
    metadata: PayloadMeta,
) -> Result<gpu::MemoryBuffer, Box<dyn Error>> {
    let buffer = allocator.allocate(metadata.size)?;
    interop.write_external_buffer(metadata.size, buffer.external_handle()?, metadata.seed)?;
    Ok(buffer)
}

fn recv_transfer(receiver: &mut Receiver, expected: PayloadMeta) -> Result<(), Box<dyn Error>> {
    let (frame, metadata) = receiver.recv::<PayloadMeta>()?;
    assert_eq!(metadata, expected);

    match (expected.kind, frame) {
        (TransferKind::Cpu, Frame::Cpu(buffer)) => {
            assert_eq!(buffer.size(), expected.size);
            assert_pattern(buffer.as_slice(), expected.seed);
        }
        (TransferKind::Gpu, Frame::Gpu(buffer)) => {
            assert_eq!(buffer.size(), expected.size);
            let bytes = GpuInterop::new(buffer.device_id())?
                .read_external_buffer(expected.size, buffer.external_handle()?)?;
            assert_pattern(&bytes, expected.seed);
        }
        (TransferKind::Cpu, Frame::Gpu(_)) => {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "expected CPU frame").into());
        }
        (TransferKind::Gpu, Frame::Cpu(_)) => {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "expected GPU frame").into());
        }
    }
    Ok(())
}

fn fill_pattern(bytes: &mut [u8], seed: u8) {
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = pattern_byte(index, seed);
    }
}

fn assert_pattern(bytes: &[u8], seed: u8) {
    for (index, actual) in bytes.iter().copied().enumerate() {
        assert_eq!(
            actual,
            pattern_byte(index, seed),
            "byte mismatch at {index}"
        );
    }
}

fn pattern_byte(index: usize, seed: u8) -> u8 {
    seed.wrapping_add((index as u8).wrapping_mul(31))
}
