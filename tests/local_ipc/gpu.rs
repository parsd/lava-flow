use lava_flow::memory::gpu;
use std::error::Error;
use std::ffi::CStr;
use std::io;

pub(crate) struct GpuInterop {
    _entry: ash::Entry,
    instance: ash::Instance,
    device: ash::Device,
    memory_properties: ash::vk::PhysicalDeviceMemoryProperties,
}

impl GpuInterop {
    pub(crate) fn new(device_id: u32) -> Result<Self, Box<dyn Error>> {
        let entry = unsafe { ash::Entry::load()? };
        let app_info = ash::vk::ApplicationInfo::default()
            .application_name(c"lava-flow-local-ipc-test")
            .application_version(0)
            .engine_name(c"lava-flow-local-ipc-test")
            .engine_version(0)
            .api_version(ash::vk::API_VERSION_1_2);
        let instance_info = ash::vk::InstanceCreateInfo::default().application_info(&app_info);
        let instance = unsafe { entry.create_instance(&instance_info, None)? };
        let setup = (|| -> Result<_, Box<dyn Error>> {
            let physical_devices = unsafe { instance.enumerate_physical_devices()? };
            let physical_device = physical_devices
                .get(device_id as usize)
                .copied()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "GPU device not found"))?;
            let queue_family_index = find_queue_family_index(&instance, physical_device)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "queue family not found"))?;
            let queue_priorities = [1.0_f32];
            let queue_create_infos = [ash::vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family_index)
                .queue_priorities(&queue_priorities)];
            let extension_names = [external_memory_extension_name().as_ptr()];
            let device_info = ash::vk::DeviceCreateInfo::default()
                .queue_create_infos(&queue_create_infos)
                .enabled_extension_names(&extension_names);
            let device = unsafe { instance.create_device(physical_device, &device_info, None)? };
            let memory_properties =
                unsafe { instance.get_physical_device_memory_properties(physical_device) };
            Ok((device, memory_properties))
        })();
        let (device, memory_properties) = match setup {
            Ok(values) => values,
            Err(source) => {
                unsafe { instance.destroy_instance(None) };
                return Err(source);
            }
        };

        Ok(Self {
            _entry: entry,
            instance,
            device,
            memory_properties,
        })
    }

    pub(crate) fn write_external_buffer(
        &self,
        size: usize,
        handle: gpu::ExternalHandle,
        seed: u8,
    ) -> Result<(), Box<dyn Error>> {
        let imported = self.import_external_buffer(size, handle)?;
        unsafe {
            let ptr = self.device.map_memory(
                imported.memory,
                0,
                size as u64,
                ash::vk::MemoryMapFlags::empty(),
            )?;
            let bytes = std::slice::from_raw_parts_mut(ptr.cast::<u8>(), size);
            crate::fill_pattern(bytes, seed);
            self.device.unmap_memory(imported.memory);
        }
        Ok(())
    }

    pub(crate) fn read_external_buffer(
        &self,
        size: usize,
        handle: gpu::ExternalHandle,
    ) -> Result<Vec<u8>, Box<dyn Error>> {
        let imported = self.import_external_buffer(size, handle)?;
        let mut bytes = vec![0_u8; size];
        unsafe {
            let ptr = self.device.map_memory(
                imported.memory,
                0,
                size as u64,
                ash::vk::MemoryMapFlags::empty(),
            )?;
            std::ptr::copy_nonoverlapping(ptr.cast::<u8>(), bytes.as_mut_ptr(), size);
            self.device.unmap_memory(imported.memory);
        }
        Ok(bytes)
    }

    fn import_external_buffer(
        &self,
        size: usize,
        handle: gpu::ExternalHandle,
    ) -> Result<ImportedGpuBuffer<'_>, Box<dyn Error>> {
        let mut external_buffer_info = ash::vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(gpu::EXTERNAL_MEMORY_HANDLE_TYPE);
        let buffer_info = ash::vk::BufferCreateInfo::default()
            .size(size as u64)
            .usage(
                ash::vk::BufferUsageFlags::TRANSFER_SRC | ash::vk::BufferUsageFlags::TRANSFER_DST,
            )
            .sharing_mode(ash::vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external_buffer_info);
        let buffer = unsafe { self.device.create_buffer(&buffer_info, None)? };
        let buffer = PendingBuffer::new(&self.device, buffer);
        let memory_requirements =
            unsafe { self.device.get_buffer_memory_requirements(buffer.as_raw()) };
        let Some(memory_type_index) = find_memory_type_index(
            &self.memory_properties,
            memory_requirements.memory_type_bits,
            ash::vk::MemoryPropertyFlags::HOST_VISIBLE
                | ash::vk::MemoryPropertyFlags::HOST_COHERENT,
        ) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "host-visible coherent memory type not found",
            )
            .into());
        };

        let memory = match self.allocate_imported_memory(
            memory_requirements.size,
            memory_type_index,
            handle,
        ) {
            Ok(memory) => PendingMemory::new(&self.device, memory),
            Err(source) => return Err(source),
        };
        if let Err(source) = unsafe {
            self.device
                .bind_buffer_memory(buffer.as_raw(), memory.as_raw(), 0)
        } {
            return Err(source.into());
        }

        Ok(ImportedGpuBuffer {
            interop: self,
            buffer: buffer.into_raw(),
            memory: memory.into_raw(),
        })
    }

    fn allocate_imported_memory(
        &self,
        allocation_size: u64,
        memory_type_index: u32,
        handle: gpu::ExternalHandle,
    ) -> Result<ash::vk::DeviceMemory, Box<dyn Error>> {
        #[cfg(unix)]
        {
            use std::os::fd::{AsRawFd, IntoRawFd};

            let fd = std::os::fd::OwnedFd::from(handle);
            let mut import_info = ash::vk::ImportMemoryFdInfoKHR::default()
                .handle_type(gpu::EXTERNAL_MEMORY_HANDLE_TYPE)
                .fd(fd.as_raw_fd());
            let alloc_info = ash::vk::MemoryAllocateInfo::default()
                .allocation_size(allocation_size)
                .memory_type_index(memory_type_index)
                .push_next(&mut import_info);
            let memory = unsafe { self.device.allocate_memory(&alloc_info, None)? };
            let _ = fd.into_raw_fd();
            Ok(memory)
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;

            let handle = std::os::windows::io::OwnedHandle::from(handle);
            let mut import_info = ash::vk::ImportMemoryWin32HandleInfoKHR::default()
                .handle_type(gpu::EXTERNAL_MEMORY_HANDLE_TYPE)
                .handle(handle.as_raw_handle() as ash::vk::HANDLE);
            let alloc_info = ash::vk::MemoryAllocateInfo::default()
                .allocation_size(allocation_size)
                .memory_type_index(memory_type_index)
                .push_next(&mut import_info);
            let memory = unsafe { self.device.allocate_memory(&alloc_info, None)? };
            Ok(memory)
        }
    }
}

impl Drop for GpuInterop {
    fn drop(&mut self) {
        unsafe {
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

struct ImportedGpuBuffer<'a> {
    interop: &'a GpuInterop,
    buffer: ash::vk::Buffer,
    memory: ash::vk::DeviceMemory,
}

impl Drop for ImportedGpuBuffer<'_> {
    fn drop(&mut self) {
        unsafe {
            self.interop.device.free_memory(self.memory, None);
            self.interop.device.destroy_buffer(self.buffer, None);
        }
    }
}

struct PendingBuffer<'a> {
    device: &'a ash::Device,
    buffer: Option<ash::vk::Buffer>,
}

impl<'a> PendingBuffer<'a> {
    fn new(device: &'a ash::Device, buffer: ash::vk::Buffer) -> Self {
        Self {
            device,
            buffer: Some(buffer),
        }
    }

    fn as_raw(&self) -> ash::vk::Buffer {
        self.buffer.expect("pending buffer")
    }

    fn into_raw(mut self) -> ash::vk::Buffer {
        self.buffer.take().expect("pending buffer")
    }
}

impl Drop for PendingBuffer<'_> {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            unsafe { self.device.destroy_buffer(buffer, None) };
        }
    }
}

struct PendingMemory<'a> {
    device: &'a ash::Device,
    memory: Option<ash::vk::DeviceMemory>,
}

impl<'a> PendingMemory<'a> {
    fn new(device: &'a ash::Device, memory: ash::vk::DeviceMemory) -> Self {
        Self {
            device,
            memory: Some(memory),
        }
    }

    fn as_raw(&self) -> ash::vk::DeviceMemory {
        self.memory.expect("pending memory")
    }

    fn into_raw(mut self) -> ash::vk::DeviceMemory {
        self.memory.take().expect("pending memory")
    }
}

impl Drop for PendingMemory<'_> {
    fn drop(&mut self) {
        if let Some(memory) = self.memory.take() {
            unsafe { self.device.free_memory(memory, None) };
        }
    }
}

fn find_queue_family_index(
    instance: &ash::Instance,
    physical_device: ash::vk::PhysicalDevice,
) -> Option<u32> {
    let families = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    families.iter().enumerate().find_map(|(index, family)| {
        family
            .queue_flags
            .contains(ash::vk::QueueFlags::TRANSFER)
            .then_some(index as u32)
    })
}

fn find_memory_type_index(
    properties: &ash::vk::PhysicalDeviceMemoryProperties,
    type_bits: u32,
    required: ash::vk::MemoryPropertyFlags,
) -> Option<u32> {
    (0..properties.memory_type_count).find(|index| {
        let supported = (type_bits & (1_u32 << *index)) != 0;
        let flags = properties.memory_types[*index as usize].property_flags;
        supported && flags.contains(required)
    })
}

fn external_memory_extension_name() -> &'static CStr {
    #[cfg(unix)]
    {
        ash::khr::external_memory_fd::NAME
    }
    #[cfg(windows)]
    {
        ash::khr::external_memory_win32::NAME
    }
}
