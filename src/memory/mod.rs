//! Layer-1 memory allocation APIs.
//!
//! This module provides a unified allocator that can create CPU and GPU buffers,
//! plus helper selection logic for scope/profile-based strategy choice.

mod allocator;
mod cpu;
mod gpu;

pub use allocator::{
    InterprocessMemoryHandle, MemoryAllocator, MemoryBuffer, MemoryLocation, MemoryType,
};
pub use cpu::CpuAllocationStrategy;
