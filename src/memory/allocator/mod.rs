#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// Unified interprocess memory handle wrapper for GPU and CPU shared memory.
#[derive(Debug)]
pub(crate) enum InterprocessMemoryHandle {
    /// Linux/Unix-like opaque file descriptor for Vulkan GPU memory.
    #[cfg(unix)]
    GpuOpaqueFd(std::os::fd::OwnedFd),
    /// Linux/Unix-like file descriptor for CPU shared-memory transport.
    #[cfg(unix)]
    CpuSharedFd(std::os::fd::OwnedFd),
    /// Windows opaque handle style identifier for Vulkan GPU memory.
    #[cfg(windows)]
    GpuOpaqueWin32Handle(std::os::windows::io::OwnedHandle),
    /// Windows mapping handle style identifier for CPU shared-memory transport.
    #[cfg(windows)]
    CpuSharedWin32Handle(std::os::windows::io::OwnedHandle),
}

#[cfg(test)]
pub(crate) mod tests {
    pub(crate) mod support {
        #[cfg(unix)]
        pub(crate) use super::super::unix::tests::support::{handle_is_cpu, handle_is_gpu};
        #[cfg(windows)]
        pub(crate) use super::super::windows::tests::support::{handle_is_cpu, handle_is_gpu};
    }
}
