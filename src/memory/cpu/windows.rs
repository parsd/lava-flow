use super::*;

use std::ffi::c_void;
use std::os::windows::io::{OwnedHandle, RawHandle};
use windows_sys::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS;

trait Syscalls: Sync {
    fn create_file_mapping(&self, size: usize) -> Result<OwnedHandle>;
    fn map_view_of_file(
        &self,
        handle: RawHandle,
        bytes: usize,
    ) -> Result<MEMORY_MAPPED_VIEW_ADDRESS>;
    fn unmap_view_of_file(&self, view: MEMORY_MAPPED_VIEW_ADDRESS);
    #[cfg_attr(not(test), allow(dead_code))]
    fn duplicate_handle_same_access(&self, source: RawHandle) -> Result<OwnedHandle>;
}

#[cfg(not(test))]
static SYSCALLS: RealSyscalls = RealSyscalls;

#[cfg(test)]
static SYSCALLS: tests::support::MockSyscalls = tests::support::MockSyscalls;

#[derive(Debug)]
pub(super) struct SharedMemoryRegion {
    ptr: *mut u8,
    len: usize,
    #[cfg_attr(not(test), allow(dead_code))]
    mapping: std::os::windows::io::OwnedHandle,
}

impl SharedMemoryRegion {
    pub(super) fn create(size: usize, max_allocation_size: usize) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;

        validate_size(size, max_allocation_size)?;
        let owned_mapping = SYSCALLS.create_file_mapping(size)?;

        let raw_view = SYSCALLS.map_view_of_file(owned_mapping.as_raw_handle(), size)?;

        Ok(Self {
            ptr: raw_view.Value.cast::<u8>(),
            len: size,
            mapping: owned_mapping,
        })
    }

    #[cfg(test)]
    pub(super) fn from_handle(
        size: usize,
        max_allocation_size: usize,
        handle: InterprocessMemoryHandle,
    ) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;

        validate_size(size, max_allocation_size)?;
        let owned_mapping = match handle {
            InterprocessMemoryHandle::CpuSharedWin32Handle(raw) => raw,
            InterprocessMemoryHandle::GpuOpaqueWin32Handle(_) => {
                return Err(LavaFlowError::UnsupportedInterprocessHandle {
                    kind: "GpuOpaqueWin32Handle",
                });
            }
        };
        let raw_view = SYSCALLS.map_view_of_file(owned_mapping.as_raw_handle(), size)?;

        Ok(Self {
            ptr: raw_view.Value.cast::<u8>(),
            len: size,
            mapping: owned_mapping,
        })
    }

    pub(super) fn as_ptr(&self) -> *const u8 {
        self.ptr.cast_const()
    }

    pub(super) fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn export_handle(&self) -> Result<InterprocessMemoryHandle> {
        use std::os::windows::io::AsRawHandle;
        let duplicated = SYSCALLS.duplicate_handle_same_access(self.mapping.as_raw_handle())?;
        Ok(InterprocessMemoryHandle::from_cpu_shared_handle(duplicated))
    }
}

impl Drop for SharedMemoryRegion {
    fn drop(&mut self) {
        if self.ptr.is_null() || self.len == 0 {
            return;
        }
        SYSCALLS.unmap_view_of_file(MEMORY_MAPPED_VIEW_ADDRESS {
            Value: self.ptr.cast::<c_void>(),
        });
        self.ptr = std::ptr::null_mut();
        self.len = 0;
    }
}

struct RealSyscalls;

impl Syscalls for RealSyscalls {
    fn create_file_mapping(&self, size: usize) -> Result<OwnedHandle> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::System::Memory::{CreateFileMappingW, PAGE_READWRITE};

        let size = size as u64;
        let max_size_high = (size >> 32) as u32;
        let max_size_low = size as u32;
        let raw = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                max_size_high,
                max_size_low,
                std::ptr::null(),
            )
        };
        if raw.is_null() {
            return Err(shared_memory_error("CreateFileMappingW"));
        }
        let owned = unsafe { OwnedHandle::from_raw_handle(raw) };
        Ok(owned)
    }

    fn map_view_of_file(
        &self,
        handle: RawHandle,
        bytes: usize,
    ) -> Result<MEMORY_MAPPED_VIEW_ADDRESS> {
        use windows_sys::Win32::System::Memory::{FILE_MAP_ALL_ACCESS, MapViewOfFile};
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, bytes) };
        if view.Value.is_null() {
            return Err(shared_memory_error("MapViewOfFile"));
        }
        Ok(view)
    }

    fn unmap_view_of_file(&self, view: MEMORY_MAPPED_VIEW_ADDRESS) {
        use windows_sys::Win32::System::Memory::UnmapViewOfFile;
        let _ = unsafe { UnmapViewOfFile(view) };
    }

    fn duplicate_handle_same_access(&self, source: RawHandle) -> Result<OwnedHandle> {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        let current_process = unsafe { GetCurrentProcess() };
        let mut duplicated = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                current_process,
                source,
                current_process,
                &mut duplicated,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            return Err(shared_memory_error("DuplicateHandle"));
        }
        let owned = unsafe { OwnedHandle::from_raw_handle(duplicated) };
        Ok(owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::io::RawHandle;

    const BUFFER_SIZE: usize = 64;
    const SMALL_SIZE: usize = 1;
    const SMALL_CAP: usize = 64;
    const OVER_CAP_SIZE: usize = 65;
    const TEST_BYTE_OFFSET: usize = 4;
    const TEST_BYTE_VALUE: u8 = 0x2a;

    fn run_with_fail<T>(op: &'static str, f: impl FnOnce() -> T) -> T {
        support::set_fail(op);
        f()
    }

    fn cpu_raw_handle(handle: &InterprocessMemoryHandle) -> RawHandle {
        match handle {
            InterprocessMemoryHandle::CpuSharedWin32Handle(owned) => owned.as_raw_handle(),
            InterprocessMemoryHandle::GpuOpaqueWin32Handle(_) => panic!("expected cpu handle"),
        }
    }

    #[test]
    fn create_and_import_round_trip_shares_bytes() {
        let mut region = SharedMemoryRegion::create(BUFFER_SIZE, hard_max_cpu_allocation_size())
            .expect("create region");
        unsafe {
            region
                .as_mut_ptr()
                .wrapping_add(TEST_BYTE_OFFSET)
                .write(TEST_BYTE_VALUE);
        }
        let handle = region.export_handle().expect("export handle");

        let imported =
            SharedMemoryRegion::from_handle(BUFFER_SIZE, hard_max_cpu_allocation_size(), handle)
                .expect("import handle");
        unsafe {
            assert_eq!(
                imported.as_ptr().wrapping_add(TEST_BYTE_OFFSET).read(),
                TEST_BYTE_VALUE
            );
        }
    }

    #[test]
    fn create_fails_for_extreme_size() {
        let err = SharedMemoryRegion::create(usize::MAX, hard_max_cpu_allocation_size())
            .expect_err("expected create failure");
        assert!(matches!(
            err,
            LavaFlowError::InvalidAllocationRequest {
                reason: AllocationReason::ExceedsMaxSize,
                ..
            }
        ));
    }

    #[test]
    fn configured_cap_is_enforced() {
        let err = SharedMemoryRegion::create(OVER_CAP_SIZE, SMALL_CAP)
            .expect_err("cap should be enforced");
        assert!(matches!(
            err,
            LavaFlowError::InvalidAllocationRequest {
                size: OVER_CAP_SIZE,
                reason: AllocationReason::ExceedsMaxSize,
            }
        ));
    }

    #[test]
    fn open_shared_region_rejects_gpu_handle() {
        let gpu_handle = InterprocessMemoryHandle::from_gpu_id(1).expect("create gpu handle");
        let err = SharedMemoryRegion::from_handle(
            BUFFER_SIZE,
            hard_max_cpu_allocation_size(),
            gpu_handle,
        )
        .expect_err("gpu handle must be rejected");
        assert!(matches!(
            err,
            LavaFlowError::UnsupportedInterprocessHandle {
                kind: "GpuOpaqueWin32Handle"
            }
        ));
    }

    #[test]
    fn open_shared_region_reports_map_error_for_closed_cpu_handle() {
        let allocator = crate::memory::cpu::Allocator::new();
        let buffer = allocator
            .allocate(BUFFER_SIZE)
            .expect("allocate cpu buffer");
        let handle = buffer.shared_handle().expect("export handle");
        let raw = cpu_raw_handle(&handle);
        let close_ok = unsafe { windows_sys::Win32::Foundation::CloseHandle(raw) };
        assert_ne!(close_ok, 0, "close duplicated handle");

        let err =
            SharedMemoryRegion::from_handle(BUFFER_SIZE, hard_max_cpu_allocation_size(), handle)
                .expect_err("closed cpu handle must fail mapping");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "MapViewOfFile",
                ..
            }
        ));
    }

    #[test]
    fn cpu_handle_match_reads_cpu_handle_branch() {
        let allocator = crate::memory::cpu::Allocator::new();
        let buffer = allocator
            .allocate(BUFFER_SIZE)
            .expect("allocate cpu buffer");
        let handle = buffer.shared_handle().expect("export handle");
        let raw = cpu_raw_handle(&handle);
        assert!(!raw.is_null());
    }

    #[test]
    fn cpu_handle_match_panics_for_gpu_handle_branch() {
        let handle = InterprocessMemoryHandle::from_gpu_id(1).expect("create gpu handle");
        let result = std::panic::catch_unwind(|| cpu_raw_handle(&handle));
        assert!(result.is_err(), "gpu handle branch should panic");
    }

    #[test]
    fn shared_memory_error_uses_generic_error_when_last_error_is_zero() {
        unsafe {
            windows_sys::Win32::Foundation::SetLastError(0);
        }
        let err = shared_memory_error("unit_test_last_error_zero");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation { source, .. }
                if source.kind() == std::io::ErrorKind::Other
        ));
    }

    #[test]
    fn create_failpoint_error_path() {
        let err = run_with_fail("CreateFileMappingW_null", || {
            SharedMemoryRegion::create(SMALL_SIZE, hard_max_cpu_allocation_size())
        })
        .expect_err("forced create failpoint should fail");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "CreateFileMappingW",
                ..
            }
        ));
    }

    #[test]
    fn duplicate_handle_failpoint_error_path() {
        let region = SharedMemoryRegion::create(BUFFER_SIZE, hard_max_cpu_allocation_size())
            .expect("create region");
        let err = run_with_fail("DuplicateHandle_zero", || region.export_handle())
            .expect_err("forced DuplicateHandle failpoint should fail");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "DuplicateHandle",
                ..
            }
        ));
    }

    #[test]
    fn create_file_mapping_real_syscall_error_path() {
        let err = RealSyscalls
            .create_file_mapping(0)
            .expect_err("zero-sized mapping should fail");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "CreateFileMappingW",
                ..
            }
        ));
    }

    #[test]
    fn duplicate_handle_real_syscall_error_path() {
        let err = RealSyscalls
            .duplicate_handle_same_access(std::ptr::null_mut())
            .expect_err("null source handle must fail");
        assert!(matches!(
            err,
            LavaFlowError::SharedMemoryOperation {
                operation: "DuplicateHandle",
                ..
            }
        ));
    }

    #[test]
    fn drop_guard_returns_for_null_pointer() {
        let mapping = RealSyscalls
            .create_file_mapping(SMALL_SIZE)
            .expect("create test mapping");
        let region = SharedMemoryRegion {
            ptr: std::ptr::null_mut(),
            len: 1,
            mapping,
        };
        drop(region);
    }

    pub(in crate::memory::cpu::windows) mod support {
        use super::*;

        thread_local! {
            static FAIL_OP_WINDOWS: std::cell::RefCell<Option<&'static str>> = const { std::cell::RefCell::new(None) };
        }

        pub fn set_fail(op: &'static str) {
            FAIL_OP_WINDOWS.with(|cell| {
                *cell.borrow_mut() = Some(op);
            });
        }

        fn should_fail(op: &'static str) -> bool {
            FAIL_OP_WINDOWS.with(|cell| {
                let mut current = cell.borrow_mut();
                if current.as_ref() == Some(&op) {
                    *current = None;
                    true
                } else {
                    false
                }
            })
        }

        pub struct MockSyscalls;

        impl Syscalls for MockSyscalls {
            fn create_file_mapping(&self, size: usize) -> Result<OwnedHandle> {
                if should_fail("CreateFileMappingW_null") {
                    Err(shared_memory_error("CreateFileMappingW"))
                } else {
                    RealSyscalls.create_file_mapping(size)
                }
            }

            fn map_view_of_file(
                &self,
                handle: RawHandle,
                bytes: usize,
            ) -> Result<MEMORY_MAPPED_VIEW_ADDRESS> {
                RealSyscalls.map_view_of_file(handle, bytes)
            }

            fn unmap_view_of_file(&self, view: MEMORY_MAPPED_VIEW_ADDRESS) {
                RealSyscalls.unmap_view_of_file(view)
            }

            fn duplicate_handle_same_access(&self, source: RawHandle) -> Result<OwnedHandle> {
                if should_fail("DuplicateHandle_zero") {
                    Err(shared_memory_error("DuplicateHandle"))
                } else {
                    RealSyscalls.duplicate_handle_same_access(source)
                }
            }
        }
    }
}
