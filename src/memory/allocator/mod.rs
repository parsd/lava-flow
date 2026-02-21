#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// Unified interprocess memory handle wrapper for GPU and CPU shared memory.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
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
