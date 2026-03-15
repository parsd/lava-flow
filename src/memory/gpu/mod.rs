use crate::error::{AllocationReason, LavaFlowError, Result};
use crate::memory::allocator::InterprocessMemoryHandle;
use ash::vk;
use std::sync::{Arc, OnceLock};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix::{EXTERNAL_MEMORY_HANDLE_TYPE, ExternalHandle, ExternalMemoryDevice};
#[cfg(windows)]
use windows::{EXTERNAL_MEMORY_HANDLE_TYPE, ExternalHandle, ExternalMemoryDevice};

const ENV_DISABLE_VULKAN: &str = "LAVA_FLOW_DISABLE_VULKAN";
const DEFAULT_DEVICE_ID: u32 = 0;
const PKG_VERSION_MAJOR: &str = env!("CARGO_PKG_VERSION_MAJOR");
const PKG_VERSION_MINOR: &str = env!("CARGO_PKG_VERSION_MINOR");
const PKG_VERSION_PATCH: &str = env!("CARGO_PKG_VERSION_PATCH");

/// GPU-backed memory buffer metadata and storage.
#[derive(Debug)]
pub struct MemoryBuffer {
    context: Arc<DeviceContext>,
    size: usize,
    allocation_size: u64,
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    #[cfg_attr(not(any(test, windows)), allow(dead_code))]
    external_handle: ExternalHandle,
}

impl MemoryBuffer {
    /// Returns the buffer size in bytes.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Returns the Vulkan-required backing allocation size in bytes (>= size()).
    pub fn allocation_size(&self) -> u64 {
        self.allocation_size
    }

    /// Returns the device identifier used for allocation.
    pub fn device_id(&self) -> u32 {
        self.context.device_id
    }

    /// Returns the exportable external handle.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn shared_handle(&self) -> Result<InterprocessMemoryHandle> {
        self.external_handle.duplicate_for_ipc()
    }
}

impl Drop for MemoryBuffer {
    fn drop(&mut self) {
        // Order because of vkBindBufferMemory buffer depends on memory:
        if self.buffer != vk::Buffer::null() {
            unsafe { self.context.device.destroy_buffer(self.buffer, None) };
            self.buffer = vk::Buffer::null();
        }
        if self.memory != vk::DeviceMemory::null() {
            unsafe { self.context.device.free_memory(self.memory, None) };
            self.memory = vk::DeviceMemory::null();
        }
        self.size = 0;
        self.allocation_size = 0;
    }
}

/// Vulkan GPU allocator.
#[derive(Debug)]
pub struct Allocator {
    // Shared ownership keeps Vulkan device state alive until the last allocated buffer is dropped.
    context: Arc<DeviceContext>,
}

impl Allocator {
    /// Creates a GPU allocator backend for logical device id `0`.
    pub fn new() -> Result<Self> {
        Self::new_for_device(DEFAULT_DEVICE_ID)
    }

    /// Creates a GPU allocator backend bound to one logical device id.
    pub fn new_for_device(device_id: u32) -> Result<Self> {
        if vulkan_disabled_by_env() {
            return Err(LavaFlowError::GpuBackendUnavailable);
        }
        let context = Arc::new(DeviceContext::new(device_id)?);
        Ok(Self { context })
    }

    /// Returns the logical device id this allocator is bound to.
    pub fn device_id(&self) -> u32 {
        self.context.device_id
    }

    /// Allocates a GPU buffer and tags it with an exportable external handle.
    pub fn allocate(&self, size: usize) -> Result<MemoryBuffer> {
        let requested_size = size;
        if requested_size == 0 {
            return Err(LavaFlowError::InvalidAllocationRequest {
                size: requested_size,
                reason: AllocationReason::ZeroSize,
            });
        }
        let context = &self.context;
        let buffer = OwnedBuffer::create(context, requested_size)?;
        let memory_requirements =
            VULKAN_API.buffer_memory_requirements(&context.device, buffer.as_raw());
        // Vulkan may require allocating more bytes than requested due to alignment/granularity.
        let allocation_size = memory_requirements.size;
        if allocation_size < requested_size as u64 {
            return Err(vulkan_operation_error(
                "get_buffer_memory_requirements",
                format!(
                    "allocation size {} smaller than requested {}",
                    allocation_size, requested_size
                ),
            ));
        }
        let memory_type_index =
            context.resolve_memory_type_index(memory_requirements.memory_type_bits)?;
        let memory =
            OwnedMemory::allocate(context, buffer.as_raw(), allocation_size, memory_type_index)?;
        VULKAN_API.bind_buffer_memory(&context.device, buffer.as_raw(), memory.as_raw())?;
        // Keep RAII guards alive until export succeeds, so failure paths cannot leak resources.
        let external_handle = VULKAN_API.export_memory_handle(context, memory.as_raw())?;

        Ok(MemoryBuffer {
            context: Arc::clone(&self.context),
            size: requested_size,
            allocation_size,
            buffer: buffer.into_raw(),
            memory: memory.into_raw(),
            external_handle,
        })
    }
}

struct DeviceContext {
    _runtime: Arc<VulkanRuntime>,
    #[cfg_attr(not(test), allow(dead_code))]
    queue_family_index: u32,
    device_id: u32,
    device: ash::Device,
    external_memory_device: ExternalMemoryDevice,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
}

impl std::fmt::Debug for DeviceContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceContext")
            .field("device_id", &self.device_id)
            .field("runtime_strong_count", &Arc::strong_count(&self._runtime))
            .finish_non_exhaustive()
    }
}

impl DeviceContext {
    fn new(device_id: u32) -> Result<Self> {
        let runtime = VulkanRuntime::instance()?;
        let physical_device = runtime.get_physical_device(device_id)?;
        let instance = &runtime.instance;
        let queue_family_index = VULKAN_API
            .find_queue_family_index(instance, physical_device)
            .ok_or_else(|| vulkan_operation_error("pick_queue_family", "no queue family found"))?;
        // One queue at priority 1.0 for buffer allocation/mapping operations.
        let queue_priorities = [1.0_f32];
        let queue_create_info = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family_index)
            .queue_priorities(&queue_priorities)];

        let extension_names = ExternalMemoryDevice::required_extensions();
        // Enable platform-specific external-memory extension for exportable handles.
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_create_info)
            .enabled_extension_names(extension_names);
        let device = VULKAN_API.create_device(instance, physical_device, &device_info)?;

        let external_memory_device = ExternalMemoryDevice::new(instance, &device);
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };
        Ok(Self {
            _runtime: runtime,
            queue_family_index,
            device_id,
            device,
            external_memory_device,
            memory_properties,
        })
    }

    fn resolve_memory_type_index(&self, type_bits: u32) -> Result<u32> {
        VULKAN_API
            .find_memory_type_index(
                &self.memory_properties,
                type_bits,
                // Host-visible + coherent keeps writes CPU-accessible without explicit flush.
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )
            .ok_or_else(|| {
                vulkan_operation_error("find_memory_type_index", "no host-visible memory type")
            })
    }
}

impl Drop for DeviceContext {
    fn drop(&mut self) {
        // `ExternalMemoryDevice` is only an ash extension wrapper (dispatch helper), not
        // an owned Vulkan object with its own destroy function. Destroying the VkDevice is
        // the required explicit teardown here; wrapper fields then drop as plain Rust data.
        unsafe { self.device.destroy_device(None) };
    }
}

struct VulkanRuntime {
    // Keep the Vulkan loader alive at least as long as the instance.
    _entry: ash::Entry,
    instance: ash::Instance,
}

impl VulkanRuntime {
    fn instance() -> Result<Arc<Self>> {
        static SHARED_RUNTIME: OnceLock<std::result::Result<Arc<VulkanRuntime>, String>> =
            OnceLock::new();
        SHARED_RUNTIME
            .get_or_init(|| {
                let entry = VULKAN_API.load_entry().map_err(|err| err.to_string())?;
                let instance = Self::create_instance(&entry).map_err(|err| err.to_string())?;
                Ok(Arc::new(Self {
                    _entry: entry,
                    instance,
                }))
            })
            .as_ref()
            .map(Arc::clone)
            .map_err(|err| vulkan_operation_error("init_runtime", err.clone()))
    }

    fn get_physical_device(&self, requested_device_id: u32) -> Result<vk::PhysicalDevice> {
        let physical_devices = VULKAN_API.enumerate_physical_devices(&self.instance)?;
        if physical_devices.is_empty() {
            return Err(LavaFlowError::GpuBackendUnavailable);
        }

        physical_devices
            .get(requested_device_id as usize)
            .copied()
            .ok_or(LavaFlowError::GpuDeviceNotFound {
                device_id: requested_device_id,
            })
    }

    fn create_instance(entry: &ash::Entry) -> Result<ash::Instance> {
        let instance_api = unsafe { entry.try_enumerate_instance_version() }
            .map_err(|err| vulkan_operation_error("enumerate_instance_version", err.to_string()))?
            .unwrap_or(vk::API_VERSION_1_0);
        Self::ensure_min_vulkan_version(instance_api)?;

        let package_version = Self::package_version();
        let app_info = vk::ApplicationInfo::default()
            .application_name(c"lava-flow")
            .application_version(package_version)
            .engine_name(c"lava-flow")
            .engine_version(package_version)
            .api_version(vk::API_VERSION_1_2);
        let instance_info = vk::InstanceCreateInfo::default().application_info(&app_info);
        VULKAN_API.create_instance(entry, &instance_info)
    }

    fn ensure_min_vulkan_version(instance_api: u32) -> Result<()> {
        if instance_api < vk::API_VERSION_1_2 {
            return Err(vulkan_operation_error(
                "check_instance_version",
                format!(
                    "requires Vulkan 1.2+, found {}.{}.{}",
                    vk::api_version_major(instance_api),
                    vk::api_version_minor(instance_api),
                    vk::api_version_patch(instance_api),
                ),
            ));
        }
        Ok(())
    }

    fn package_version() -> u32 {
        let major = PKG_VERSION_MAJOR.parse::<u32>().unwrap_or(0);
        let minor = PKG_VERSION_MINOR.parse::<u32>().unwrap_or(0);
        let patch = PKG_VERSION_PATCH.parse::<u32>().unwrap_or(0);
        vk::make_api_version(0, major, minor, patch)
    }
}

impl Drop for VulkanRuntime {
    fn drop(&mut self) {
        unsafe { self.instance.destroy_instance(None) };
    }
}

struct OwnedBuffer<'a> {
    context: &'a DeviceContext,
    buffer: vk::Buffer,
}

impl<'a> OwnedBuffer<'a> {
    fn create(context: &'a DeviceContext, size: usize) -> Result<Self> {
        let mut external_buffer_info =
            // Export handle metadata is attached via pNext for buffer creation.
            vk::ExternalMemoryBufferCreateInfo::default().handle_types(EXTERNAL_MEMORY_HANDLE_TYPE);
        let buffer_create_info = vk::BufferCreateInfo::default()
            .size(size as u64)
            // Transfer usage covers staging-like source/destination copies.
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);
        let buffer = unsafe { context.device.create_buffer(&buffer_create_info, None) }
            .map_err(|err| vulkan_operation_error("create_buffer", err.to_string()))?;
        Ok(Self { context, buffer })
    }

    fn as_raw(&self) -> vk::Buffer {
        self.buffer
    }

    fn into_raw(mut self) -> vk::Buffer {
        let raw = self.buffer;
        self.buffer = vk::Buffer::null();
        raw
    }
}

impl Drop for OwnedBuffer<'_> {
    fn drop(&mut self) {
        if self.buffer != vk::Buffer::null() {
            unsafe { self.context.device.destroy_buffer(self.buffer, None) };
        }
    }
}

struct OwnedMemory<'a> {
    context: &'a DeviceContext,
    memory: vk::DeviceMemory,
}

impl<'a> OwnedMemory<'a> {
    fn allocate(
        context: &'a DeviceContext,
        buffer: vk::Buffer,
        allocation_size: u64,
        memory_type_index: u32,
    ) -> Result<Self> {
        let mut export_memory_info =
            // Marks memory as exportable through external-handle APIs.
            vk::ExportMemoryAllocateInfo::default().handle_types(EXTERNAL_MEMORY_HANDLE_TYPE);
        let mut dedicated_info = vk::MemoryDedicatedAllocateInfo::default().buffer(buffer);
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(allocation_size)
            .memory_type_index(memory_type_index)
            .push_next(&mut export_memory_info)
            .push_next(&mut dedicated_info);
        let memory = VULKAN_API.allocate_memory(&context.device, &alloc_info)?;
        Ok(Self { context, memory })
    }

    fn as_raw(&self) -> vk::DeviceMemory {
        self.memory
    }

    fn into_raw(mut self) -> vk::DeviceMemory {
        let raw = self.memory;
        self.memory = vk::DeviceMemory::null();
        raw
    }
}

impl Drop for OwnedMemory<'_> {
    fn drop(&mut self) {
        if self.memory != vk::DeviceMemory::null() {
            unsafe { self.context.device.free_memory(self.memory, None) };
        }
    }
}

trait VulkanApi: Sync {
    fn load_entry(&self) -> Result<ash::Entry>;
    fn create_instance(
        &self,
        entry: &ash::Entry,
        instance_info: &vk::InstanceCreateInfo<'_>,
    ) -> Result<ash::Instance>;
    fn enumerate_physical_devices(
        &self,
        instance: &ash::Instance,
    ) -> Result<Vec<vk::PhysicalDevice>>;
    fn find_queue_family_index(
        &self,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
    ) -> Option<u32>;
    fn create_device(
        &self,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        device_info: &vk::DeviceCreateInfo<'_>,
    ) -> Result<ash::Device>;
    fn find_memory_type_index(
        &self,
        properties: &vk::PhysicalDeviceMemoryProperties,
        type_bits: u32,
        required_flags: vk::MemoryPropertyFlags,
    ) -> Option<u32>;
    fn allocate_memory(
        &self,
        device: &ash::Device,
        alloc_info: &vk::MemoryAllocateInfo<'_>,
    ) -> Result<vk::DeviceMemory>;
    fn buffer_memory_requirements(
        &self,
        device: &ash::Device,
        buffer: vk::Buffer,
    ) -> vk::MemoryRequirements;
    fn bind_buffer_memory(
        &self,
        device: &ash::Device,
        buffer: vk::Buffer,
        memory: vk::DeviceMemory,
    ) -> Result<()>;
    fn export_memory_handle(
        &self,
        context: &DeviceContext,
        memory: vk::DeviceMemory,
    ) -> Result<ExternalHandle>;
}

struct RealVulkanApi;

impl VulkanApi for RealVulkanApi {
    fn load_entry(&self) -> Result<ash::Entry> {
        unsafe { ash::Entry::load() }
            .map_err(|err| vulkan_operation_error("load_entry", err.to_string()))
    }

    fn create_instance(
        &self,
        entry: &ash::Entry,
        instance_info: &vk::InstanceCreateInfo<'_>,
    ) -> Result<ash::Instance> {
        unsafe { entry.create_instance(instance_info, None) }
            .map_err(|err| vulkan_operation_error("create_instance", err.to_string()))
    }

    fn enumerate_physical_devices(
        &self,
        instance: &ash::Instance,
    ) -> Result<Vec<vk::PhysicalDevice>> {
        unsafe { instance.enumerate_physical_devices() }
            .map_err(|err| vulkan_operation_error("enumerate_physical_devices", err.to_string()))
    }

    fn find_queue_family_index(
        &self,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
    ) -> Option<u32> {
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let queue_family_is_usable = |props: &vk::QueueFamilyProperties| {
            props.queue_count > 0
                && props
                    .queue_flags
                    .intersects(vk::QueueFlags::COMPUTE | vk::QueueFlags::GRAPHICS)
        };
        queue_families
            .iter()
            .position(queue_family_is_usable)
            .and_then(|index| u32::try_from(index).ok())
    }

    fn create_device(
        &self,
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        device_info: &vk::DeviceCreateInfo<'_>,
    ) -> Result<ash::Device> {
        unsafe { instance.create_device(physical_device, device_info, None) }
            .map_err(|err| vulkan_operation_error("create_device", err.to_string()))
    }

    fn find_memory_type_index(
        &self,
        properties: &vk::PhysicalDeviceMemoryProperties,
        type_bits: u32,
        required_flags: vk::MemoryPropertyFlags,
    ) -> Option<u32> {
        let count = usize::try_from(properties.memory_type_count).ok()?;
        properties.memory_types[..count]
            .iter()
            .enumerate()
            .find_map(|(index, memory_type)| {
                let bit = 1_u32.checked_shl(u32::try_from(index).ok()?)?;
                let supported = (type_bits & bit) != 0;
                if supported && memory_type.property_flags.contains(required_flags) {
                    u32::try_from(index).ok()
                } else {
                    None
                }
            })
    }

    fn allocate_memory(
        &self,
        device: &ash::Device,
        alloc_info: &vk::MemoryAllocateInfo<'_>,
    ) -> Result<vk::DeviceMemory> {
        unsafe { device.allocate_memory(alloc_info, None) }
            .map_err(|err| vulkan_operation_error("allocate_memory", err.to_string()))
    }

    fn buffer_memory_requirements(
        &self,
        device: &ash::Device,
        buffer: vk::Buffer,
    ) -> vk::MemoryRequirements {
        unsafe { device.get_buffer_memory_requirements(buffer) }
    }

    fn bind_buffer_memory(
        &self,
        device: &ash::Device,
        buffer: vk::Buffer,
        memory: vk::DeviceMemory,
    ) -> Result<()> {
        unsafe { device.bind_buffer_memory(buffer, memory, 0) }
            .map_err(|err| vulkan_operation_error("bind_buffer_memory", err.to_string()))
    }

    fn export_memory_handle(
        &self,
        context: &DeviceContext,
        memory: vk::DeviceMemory,
    ) -> Result<ExternalHandle> {
        context.export_memory_handle(memory)
    }
}

#[cfg(not(test))]
static VULKAN_API: RealVulkanApi = RealVulkanApi;

#[cfg(test)]
static VULKAN_API: tests::support::MockVulkanApi = tests::support::MockVulkanApi;

fn vulkan_disabled_by_env() -> bool {
    std::env::var_os(ENV_DISABLE_VULKAN).is_some()
}

fn vulkan_operation_error(operation: &'static str, details: impl Into<String>) -> LavaFlowError {
    LavaFlowError::VulkanOperation {
        operation,
        details: details.into(),
    }
}

#[cfg(test)]
mod tests {
    use self::support::set_fail_point;
    use super::*;
    use crate::test_support::env::Guard as EnvGuard;
    use ash::vk::Handle;
    use std::sync::Once;

    const BUFFER_SIZE: usize = 64;
    const UNKNOWN_DEVICE_ID: u32 = 99;
    static VULKAN_SKIP_NOTICE: Once = Once::new();

    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub(super) enum FailPoint {
        NoPhysicalDevice,
        NoQueueFamily,
        CreateInstance,
        EnumeratePhysicalDevicesError,
        CreateDevice,
        LoadEntry,
        FindMemoryType,
        AllocateMemory,
        BufferRequirementsTooSmall,
        BindMemory,
        ExportHandle,
        ExportHandleSyscall,
    }

    pub(super) mod support {
        use super::*;
        use std::{cell::Cell, thread_local};

        thread_local! {
            static FAIL_POINT: Cell<Option<FailPoint>> = const { Cell::new(None) };
        }

        pub(super) fn set_fail_point(fail_point: FailPoint) {
            FAIL_POINT.with(|slot| slot.set(Some(fail_point)));
        }

        fn should_fail(fail_point: FailPoint) -> bool {
            FAIL_POINT.with(|slot| {
                if slot.get() == Some(fail_point) {
                    slot.set(None);
                    true
                } else {
                    false
                }
            })
        }

        pub(in crate::memory::gpu) struct MockVulkanApi;

        impl VulkanApi for MockVulkanApi {
            fn load_entry(&self) -> Result<ash::Entry> {
                if should_fail(FailPoint::LoadEntry) {
                    Err(vulkan_operation_error(
                        "load_entry",
                        vk::Result::ERROR_INITIALIZATION_FAILED.to_string(),
                    ))
                } else {
                    RealVulkanApi.load_entry()
                }
            }

            fn create_instance(
                &self,
                entry: &ash::Entry,
                instance_info: &vk::InstanceCreateInfo<'_>,
            ) -> Result<ash::Instance> {
                if should_fail(FailPoint::CreateInstance) {
                    Err(vulkan_operation_error(
                        "create_instance",
                        vk::Result::ERROR_INITIALIZATION_FAILED.to_string(),
                    ))
                } else {
                    RealVulkanApi.create_instance(entry, instance_info)
                }
            }

            fn enumerate_physical_devices(
                &self,
                instance: &ash::Instance,
            ) -> Result<Vec<vk::PhysicalDevice>> {
                if should_fail(FailPoint::EnumeratePhysicalDevicesError) {
                    return Err(vulkan_operation_error(
                        "enumerate_physical_devices",
                        vk::Result::ERROR_INITIALIZATION_FAILED.to_string(),
                    ));
                }
                if should_fail(FailPoint::NoPhysicalDevice) {
                    Ok(Vec::new())
                } else {
                    RealVulkanApi.enumerate_physical_devices(instance)
                }
            }

            fn find_queue_family_index(
                &self,
                instance: &ash::Instance,
                physical_device: vk::PhysicalDevice,
            ) -> Option<u32> {
                if should_fail(FailPoint::NoQueueFamily) {
                    None
                } else {
                    RealVulkanApi.find_queue_family_index(instance, physical_device)
                }
            }

            fn create_device(
                &self,
                instance: &ash::Instance,
                physical_device: vk::PhysicalDevice,
                device_info: &vk::DeviceCreateInfo<'_>,
            ) -> Result<ash::Device> {
                if should_fail(FailPoint::CreateDevice) {
                    Err(vulkan_operation_error(
                        "create_device",
                        vk::Result::ERROR_INITIALIZATION_FAILED.to_string(),
                    ))
                } else {
                    RealVulkanApi.create_device(instance, physical_device, device_info)
                }
            }

            fn find_memory_type_index(
                &self,
                properties: &vk::PhysicalDeviceMemoryProperties,
                type_bits: u32,
                required_flags: vk::MemoryPropertyFlags,
            ) -> Option<u32> {
                if should_fail(FailPoint::FindMemoryType) {
                    None
                } else {
                    RealVulkanApi.find_memory_type_index(properties, type_bits, required_flags)
                }
            }

            fn allocate_memory(
                &self,
                device: &ash::Device,
                alloc_info: &vk::MemoryAllocateInfo<'_>,
            ) -> Result<vk::DeviceMemory> {
                if should_fail(FailPoint::AllocateMemory) {
                    Err(vulkan_operation_error(
                        "allocate_memory",
                        vk::Result::ERROR_OUT_OF_DEVICE_MEMORY.to_string(),
                    ))
                } else {
                    RealVulkanApi.allocate_memory(device, alloc_info)
                }
            }

            fn buffer_memory_requirements(
                &self,
                device: &ash::Device,
                buffer: vk::Buffer,
            ) -> vk::MemoryRequirements {
                let mut requirements = RealVulkanApi.buffer_memory_requirements(device, buffer);
                if should_fail(FailPoint::BufferRequirementsTooSmall) && requirements.size > 0 {
                    requirements.size -= 1;
                }
                requirements
            }

            fn bind_buffer_memory(
                &self,
                device: &ash::Device,
                buffer: vk::Buffer,
                memory: vk::DeviceMemory,
            ) -> Result<()> {
                if should_fail(FailPoint::BindMemory) {
                    Err(vulkan_operation_error(
                        "bind_buffer_memory",
                        vk::Result::ERROR_MEMORY_MAP_FAILED.to_string(),
                    ))
                } else {
                    RealVulkanApi.bind_buffer_memory(device, buffer, memory)
                }
            }

            fn export_memory_handle(
                &self,
                context: &DeviceContext,
                memory: vk::DeviceMemory,
            ) -> Result<ExternalHandle> {
                if should_fail(FailPoint::ExportHandle) {
                    return Err(vulkan_operation_error(
                        "export_memory_handle",
                        "forced test failure",
                    ));
                }
                if should_fail(FailPoint::ExportHandleSyscall) {
                    #[cfg(unix)]
                    return Err(vulkan_operation_error(
                        "get_memory_fd",
                        vk::Result::ERROR_INVALID_EXTERNAL_HANDLE.to_string(),
                    ));
                    #[cfg(windows)]
                    return Err(vulkan_operation_error(
                        "get_memory_win32_handle",
                        vk::Result::ERROR_INVALID_EXTERNAL_HANDLE.to_string(),
                    ));
                }
                RealVulkanApi.export_memory_handle(context, memory)
            }
        }
    }

    fn maybe_allocator() -> Option<Allocator> {
        match Allocator::new() {
            Ok(allocator) => Some(allocator),
            Err(_) => {
                VULKAN_SKIP_NOTICE.call_once(|| {
                    eprintln!(
                        "Skipping Vulkan-dependent GPU tests: Vulkan backend unavailable (driver/loader/runtime missing or disabled via LAVA_FLOW_DISABLE_VULKAN)."
                    );
                });
                None
            }
        }
    }

    fn with_allocator(test: impl FnOnce(&mut Allocator)) {
        if let Some(mut allocator) = maybe_allocator() {
            test(&mut allocator);
        }
    }

    fn allocator_instance_handle(allocator: &Allocator) -> u64 {
        allocator.context._runtime.instance.handle().as_raw()
    }

    struct OwnedInstance {
        instance: ash::Instance,
    }

    impl OwnedInstance {
        fn new(instance: ash::Instance) -> Self {
            Self { instance }
        }

        fn as_ref(&self) -> &ash::Instance {
            &self.instance
        }
    }

    impl Drop for OwnedInstance {
        fn drop(&mut self) {
            unsafe { self.instance.destroy_instance(None) };
        }
    }

    fn available_device_ids() -> Vec<u32> {
        if vulkan_disabled_by_env() {
            return Vec::new();
        }

        let Ok(entry) = VULKAN_API.load_entry() else {
            return Vec::new();
        };
        let app_info = vk::ApplicationInfo::default()
            .application_name(c"lava-flow")
            .application_version(0)
            .engine_name(c"lava-flow")
            .engine_version(0)
            .api_version(vk::API_VERSION_1_2);
        let instance_info = vk::InstanceCreateInfo::default().application_info(&app_info);
        let Ok(instance) = VULKAN_API.create_instance(&entry, &instance_info) else {
            return Vec::new();
        };
        let instance = OwnedInstance::new(instance);
        let Ok(physical_devices) = VULKAN_API.enumerate_physical_devices(instance.as_ref()) else {
            return Vec::new();
        };
        (0..physical_devices.len())
            .map(|index| u32::try_from(index).expect("enumerated device index must fit into u32"))
            .collect::<Vec<u32>>()
    }

    #[test]
    fn available_device_ids_returns_empty_when_disabled() {
        let _guard = EnvGuard::set(ENV_DISABLE_VULKAN, "1");
        assert!(available_device_ids().is_empty());
    }

    #[test]
    fn available_device_ids_returns_empty_when_entry_load_fails() {
        set_fail_point(FailPoint::LoadEntry);
        assert!(available_device_ids().is_empty());
    }

    #[test]
    fn available_device_ids_returns_empty_when_instance_create_fails() {
        set_fail_point(FailPoint::CreateInstance);
        assert!(available_device_ids().is_empty());
    }

    #[test]
    fn available_device_ids_returns_empty_when_enumerate_fails() {
        set_fail_point(FailPoint::EnumeratePhysicalDevicesError);
        assert!(available_device_ids().is_empty());
    }

    #[test]
    fn allocate_rejects_zero_size() {
        with_allocator(|allocator| {
            let err = allocator
                .allocate(0)
                .expect_err("zero-sized allocation must fail");
            assert!(matches!(
                err,
                LavaFlowError::InvalidAllocationRequest {
                    size: 0,
                    reason: AllocationReason::ZeroSize,
                }
            ));
        });
    }

    #[test]
    fn new_reports_backend_unavailable_when_disabled() {
        let _guard = EnvGuard::set(ENV_DISABLE_VULKAN, "1");
        let err = Allocator::new().expect_err("constructor should fail without backend");
        assert!(matches!(err, LavaFlowError::GpuBackendUnavailable));
    }

    #[test]
    fn allocator_reports_selected_device_id() {
        with_allocator(|allocator| {
            let selected_device = allocator.device_id();
            assert_ne!(selected_device, UNKNOWN_DEVICE_ID);
        });
    }

    #[test]
    fn creates_allocator_per_discovered_device_and_allocates() {
        with_allocator(|_| {
            for device_id in available_device_ids() {
                let per_device =
                    Allocator::new_for_device(device_id).expect("create per-device allocator");
                assert_eq!(per_device.device_id(), device_id);
                let buffer = per_device
                    .allocate(BUFFER_SIZE)
                    .expect("allocate on selected device");
                assert_eq!(buffer.device_id(), device_id);
                assert_eq!(buffer.size(), BUFFER_SIZE);
            }
        });
    }

    #[test]
    fn allocators_share_runtime_instance_for_same_device() {
        with_allocator(|first| {
            let second = Allocator::new().expect("create second allocator");
            assert_eq!(
                allocator_instance_handle(first),
                allocator_instance_handle(&second)
            );
        });
    }

    #[test]
    fn allocators_share_runtime_instance_across_devices() {
        with_allocator(|_| {
            let ids = available_device_ids();
            let first = Allocator::new_for_device(ids[0]).expect("create first allocator");
            let second_device_id = ids.get(1).copied().unwrap_or(ids[0]);
            let second =
                Allocator::new_for_device(second_device_id).expect("create second allocator");
            assert_eq!(
                allocator_instance_handle(&first),
                allocator_instance_handle(&second)
            );
        });
    }

    #[test]
    fn allocate_rejects_unknown_device() {
        with_allocator(|_| {
            let err =
                Allocator::new_for_device(UNKNOWN_DEVICE_ID).expect_err("unknown device must fail");
            assert!(matches!(
                err,
                LavaFlowError::GpuDeviceNotFound {
                    device_id: UNKNOWN_DEVICE_ID,
                }
            ));
        });
    }

    #[test]
    fn allocate_returns_buffer_with_valid_handle() {
        with_allocator(|allocator| {
            let buffer = allocator
                .allocate(BUFFER_SIZE)
                .expect("allocate gpu buffer");
            assert!(format!("{buffer:?}").contains("MemoryBuffer"));
            assert_eq!(buffer.size(), BUFFER_SIZE);
            assert!(buffer.allocation_size() >= BUFFER_SIZE as u64);
            assert_eq!(buffer.device_id(), allocator.device_id());
            let handle = buffer.shared_handle().expect("export handle");
            #[cfg(unix)]
            assert!(matches!(handle, InterprocessMemoryHandle::GpuOpaqueFd(_)));
            #[cfg(windows)]
            assert!(matches!(
                handle,
                InterprocessMemoryHandle::GpuOpaqueWin32Handle(_)
            ));
        });
    }

    #[test]
    fn command_copy_between_allocated_buffers_succeeds() {
        with_allocator(|allocator| {
            let src = allocator
                .allocate(BUFFER_SIZE)
                .expect("allocate source gpu buffer");
            let dst = allocator
                .allocate(BUFFER_SIZE)
                .expect("allocate destination gpu buffer");
            let context = &allocator.context;
            unsafe {
                let queue = context
                    .device
                    .get_device_queue(context.queue_family_index, 0);

                let pool_info = vk::CommandPoolCreateInfo::default()
                    .queue_family_index(context.queue_family_index);
                let command_pool = context
                    .device
                    .create_command_pool(&pool_info, None)
                    .expect("create command pool");

                let alloc_info = vk::CommandBufferAllocateInfo::default()
                    .command_pool(command_pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1);
                let command_buffer = context
                    .device
                    .allocate_command_buffers(&alloc_info)
                    .expect("allocate command buffer")[0];

                let begin_info = vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
                context
                    .device
                    .begin_command_buffer(command_buffer, &begin_info)
                    .expect("begin command buffer");

                let copy_regions = [vk::BufferCopy::default().size(BUFFER_SIZE as u64)];
                context.device.cmd_copy_buffer(
                    command_buffer,
                    src.buffer,
                    dst.buffer,
                    &copy_regions,
                );
                context
                    .device
                    .end_command_buffer(command_buffer)
                    .expect("end command buffer");

                let command_buffers = [command_buffer];
                let submit_infos = [vk::SubmitInfo::default().command_buffers(&command_buffers)];
                context
                    .device
                    .queue_submit(queue, &submit_infos, vk::Fence::null())
                    .expect("submit copy command");
                context
                    .device
                    .queue_wait_idle(queue)
                    .expect("wait queue idle");

                context
                    .device
                    .free_command_buffers(command_pool, &command_buffers);
                context.device.destroy_command_pool(command_pool, None);
            }
        });
    }

    #[test]
    fn find_memory_type_index_returns_none_when_bits_do_not_match() {
        let mut properties = vk::PhysicalDeviceMemoryProperties {
            memory_type_count: 1,
            ..Default::default()
        };
        properties.memory_types[0].property_flags =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let index = RealVulkanApi.find_memory_type_index(
            &properties,
            0,
            vk::MemoryPropertyFlags::HOST_VISIBLE,
        );
        assert_eq!(index, None);
    }

    #[test]
    fn find_memory_type_index_returns_matching_slot() {
        let mut properties = vk::PhysicalDeviceMemoryProperties {
            memory_type_count: 2,
            ..Default::default()
        };
        properties.memory_types[0].property_flags = vk::MemoryPropertyFlags::DEVICE_LOCAL;
        properties.memory_types[1].property_flags =
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let index = RealVulkanApi.find_memory_type_index(
            &properties,
            0b10,
            vk::MemoryPropertyFlags::HOST_VISIBLE,
        );
        assert_eq!(index, Some(1));
    }

    #[test]
    fn vulkan_operation_error_keeps_operation_name() {
        let err = vulkan_operation_error("unit_test", "details");
        assert!(matches!(
            err,
            LavaFlowError::VulkanOperation { operation, .. } if operation == "unit_test"
        ));
    }

    #[test]
    fn new_allocator_initializes() {
        with_allocator(|allocator| {
            assert_eq!(
                allocator.device_id(),
                Allocator::new()
                    .expect("vulkan backend must be available")
                    .device_id()
            );
        });
    }

    #[test]
    fn ensure_min_vulkan_version_rejects_older_versions() {
        let err = VulkanRuntime::ensure_min_vulkan_version(vk::API_VERSION_1_1)
            .expect_err("vulkan 1.1 should be rejected");
        assert!(matches!(
            err,
            LavaFlowError::VulkanOperation {
                operation: "check_instance_version",
                ..
            }
        ));
    }

    #[test]
    fn ensure_min_vulkan_version_accepts_1_2() {
        VulkanRuntime::ensure_min_vulkan_version(vk::API_VERSION_1_2)
            .expect("vulkan 1.2 should be accepted");
    }

    #[test]
    fn select_physical_device_returns_requested_item() {
        with_allocator(|allocator| {
            let selected = Allocator::new_for_device(allocator.device_id())
                .expect("create allocator for discovered id");
            assert_eq!(selected.device_id(), allocator.device_id());
        });
    }

    #[test]
    fn select_physical_device_rejects_unknown_requested_index() {
        let err = Allocator::new_for_device(UNKNOWN_DEVICE_ID).expect_err("unknown index");
        assert!(matches!(
            err,
            LavaFlowError::GpuDeviceNotFound {
                device_id: UNKNOWN_DEVICE_ID
            }
        ));
    }

    #[test]
    fn allocate_reports_forced_find_memory_type_failure() {
        with_allocator(|allocator| {
            set_fail_point(FailPoint::FindMemoryType);
            let err = allocator
                .allocate(BUFFER_SIZE)
                .expect_err("forced memory type failure");
            assert!(matches!(
                err,
                LavaFlowError::VulkanOperation {
                    operation: "find_memory_type_index",
                    ..
                }
            ));
        });
    }

    #[test]
    fn allocate_reports_forced_allocate_memory_failure() {
        with_allocator(|allocator| {
            set_fail_point(FailPoint::AllocateMemory);
            let err = allocator
                .allocate(BUFFER_SIZE)
                .expect_err("forced allocate memory failure");
            assert!(matches!(
                err,
                LavaFlowError::VulkanOperation {
                    operation: "allocate_memory",
                    ..
                }
            ));
        });
    }

    #[test]
    fn allocate_reports_buffer_requirements_smaller_than_requested() {
        with_allocator(|allocator| {
            set_fail_point(FailPoint::BufferRequirementsTooSmall);
            let err = allocator
                .allocate(BUFFER_SIZE)
                .expect_err("forced small requirements failure");
            assert!(matches!(
                err,
                LavaFlowError::VulkanOperation {
                    operation: "get_buffer_memory_requirements",
                    ..
                }
            ));
        });
    }

    #[test]
    fn allocate_reports_forced_bind_memory_failure() {
        with_allocator(|allocator| {
            set_fail_point(FailPoint::BindMemory);
            let err = allocator
                .allocate(BUFFER_SIZE)
                .expect_err("forced bind memory failure");
            assert!(matches!(
                err,
                LavaFlowError::VulkanOperation {
                    operation: "bind_buffer_memory",
                    ..
                }
            ));
        });
    }

    #[test]
    fn allocate_reports_forced_export_failure() {
        with_allocator(|allocator| {
            set_fail_point(FailPoint::ExportHandle);
            let err = allocator
                .allocate(BUFFER_SIZE)
                .expect_err("forced export failure");
            assert!(matches!(
                err,
                LavaFlowError::VulkanOperation {
                    operation: "export_memory_handle",
                    ..
                }
            ));
        });
    }

    #[test]
    fn allocate_reports_forced_export_syscall_failure() {
        with_allocator(|allocator| {
            set_fail_point(FailPoint::ExportHandleSyscall);
            let err = allocator
                .allocate(BUFFER_SIZE)
                .expect_err("forced export syscall failure");
            #[cfg(windows)]
            assert!(matches!(
                err,
                LavaFlowError::VulkanOperation {
                    operation: "get_memory_win32_handle",
                    ..
                }
            ));
            #[cfg(unix)]
            assert!(matches!(
                err,
                LavaFlowError::VulkanOperation {
                    operation: "get_memory_fd",
                    ..
                }
            ));
        });
    }

    #[test]
    fn maybe_allocator_logs_skip_when_disabled() {
        let _guard = EnvGuard::set(ENV_DISABLE_VULKAN, "1");
        assert!(maybe_allocator().is_none());
    }

    #[test]
    fn vulkan_runtime_drop_is_exercised() {
        with_allocator(|_| {
            let entry = unsafe { ash::Entry::load() }.expect("load entry");
            let instance = VulkanRuntime::create_instance(&entry).expect("create instance");
            let runtime = VulkanRuntime {
                _entry: entry,
                instance,
            };
            drop(runtime);
        });
    }

    #[test]
    fn external_memory_device_debug_impl_is_used() {
        with_allocator(|allocator| {
            let debug_text = format!("{:?}", allocator.context.external_memory_device);
            assert!(debug_text.contains("ExternalMemoryDevice"));
        });
    }

    #[test]
    fn device_context_new_reports_forced_create_device_failure() {
        set_fail_point(FailPoint::CreateDevice);
        let result = DeviceContext::new(DEFAULT_DEVICE_ID);
        assert!(result.is_err(), "forced create device failure");
        let err = result.expect_err("error is present");
        assert!(matches!(
            err,
            LavaFlowError::VulkanOperation {
                operation: "create_device",
                ..
            }
        ));
    }

    #[test]
    fn new_returns_error_when_forcing_create_device_failure() {
        set_fail_point(FailPoint::CreateDevice);
        assert!(Allocator::new().is_err());
    }

    #[test]
    fn new_returns_error_when_forcing_no_physical_device() {
        set_fail_point(FailPoint::NoPhysicalDevice);
        assert!(Allocator::new().is_err());
    }

    #[test]
    fn new_returns_error_when_forcing_no_queue_family() {
        set_fail_point(FailPoint::NoQueueFamily);
        assert!(Allocator::new().is_err());
    }

    #[test]
    fn device_context_new_reports_no_physical_device_failure() {
        set_fail_point(FailPoint::NoPhysicalDevice);
        let result = DeviceContext::new(DEFAULT_DEVICE_ID);
        assert!(result.is_err(), "forced no physical device failure");
        let err = result.expect_err("error is present");
        assert!(matches!(err, LavaFlowError::GpuBackendUnavailable));
    }

    #[test]
    fn device_context_new_reports_no_queue_family_failure() {
        set_fail_point(FailPoint::NoQueueFamily);
        let result = DeviceContext::new(DEFAULT_DEVICE_ID);
        assert!(result.is_err(), "forced no queue family failure");
        let err = result.expect_err("error is present");
        assert!(matches!(
            err,
            LavaFlowError::VulkanOperation {
                operation: "pick_queue_family",
                ..
            }
        ));
    }
}
