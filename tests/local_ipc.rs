#[path = "local_ipc/gpu.rs"]
mod gpu_support;
#[path = "local_ipc/harness.rs"]
mod harness;

use gpu_support::GpuInterop;
use harness::{BUILD_TIMEOUT, ChildConfig, ReadyFile, run_interprocess_case};
use lava_flow::channel::{Builder, MetadataEncoding};
use lava_flow::memory::{cpu, gpu};
use lava_flow::types::ChannelId;
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::io;

#[derive(Clone, Copy)]
pub(crate) enum IpcCase {
    Cpu,
    Gpu,
}

impl IpcCase {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        }
    }
}

#[derive(Debug, Deserialize, PartialEq, Serialize)]
struct PayloadMeta {
    case: String,
    size: usize,
    seed: u8,
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
fn local_ipc_child_entry() -> Result<(), Box<dyn Error>> {
    let Some(config) = ChildConfig::new()? else {
        return Ok(());
    };

    match (config.role.as_str(), config.case.as_str()) {
        ("sender", "cpu") => run_cpu_sender(config.channel_id, config.size, config.seed),
        ("receiver", "cpu") => run_cpu_receiver(config.channel_id, config.size, config.seed),
        ("sender", "gpu") => run_gpu_sender(config.channel_id, config.size, config.seed),
        ("receiver", "gpu") => run_gpu_receiver(config.channel_id, config.size, config.seed),
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
    let mut buffer = allocator.allocate(size)?;
    fill_pattern(buffer.as_mut_slice(), seed);

    let metadata = PayloadMeta {
        case: "cpu".into(),
        size,
        seed,
    };
    ReadyFile::signal()?;
    let mut sender = Builder::local_sender(channel_id)?
        .with_metadata_encoding(MetadataEncoding::Json)
        .build_with_timeout(BUILD_TIMEOUT)?;
    sender.send(buffer, &metadata)?;
    Ok(())
}

fn run_cpu_receiver(channel_id: ChannelId, size: usize, seed: u8) -> Result<(), Box<dyn Error>> {
    let mut receiver = Builder::local_receiver(channel_id)?.build_with_timeout(BUILD_TIMEOUT)?;
    let (frame, metadata) = receiver.recv::<PayloadMeta>()?;
    assert_eq!(
        metadata,
        PayloadMeta {
            case: "cpu".into(),
            size,
            seed
        }
    );
    let buffer = frame
        .into_cpu()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "expected received CPU frame"))?;
    assert_eq!(buffer.size(), size);
    assert_pattern(buffer.as_slice(), seed);
    Ok(())
}

fn run_gpu_sender(channel_id: ChannelId, size: usize, seed: u8) -> Result<(), Box<dyn Error>> {
    let allocator = gpu::Allocator::new()?;
    let buffer = allocator.allocate(size)?;
    GpuInterop::new(allocator.device_id())?.write_external_buffer(
        size,
        buffer.external_handle()?,
        seed,
    )?;

    let metadata = PayloadMeta {
        case: "gpu".into(),
        size,
        seed,
    };
    ReadyFile::signal()?;
    let mut sender = Builder::local_sender(channel_id)?
        .with_metadata_encoding(MetadataEncoding::Json)
        .build_with_timeout(BUILD_TIMEOUT)?;
    sender.send(buffer, &metadata)?;
    Ok(())
}

fn run_gpu_receiver(channel_id: ChannelId, size: usize, seed: u8) -> Result<(), Box<dyn Error>> {
    let mut receiver = Builder::local_receiver(channel_id)?.build_with_timeout(BUILD_TIMEOUT)?;
    let (frame, metadata) = receiver.recv::<PayloadMeta>()?;
    assert_eq!(
        metadata,
        PayloadMeta {
            case: "gpu".into(),
            size,
            seed
        }
    );
    let buffer = frame
        .into_gpu()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "expected received GPU frame"))?;
    assert_eq!(buffer.size(), size);

    let bytes = GpuInterop::new(buffer.device_id())?
        .read_external_buffer(size, buffer.external_handle()?)?;
    assert_pattern(&bytes, seed);
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
