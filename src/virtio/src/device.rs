use super::block::virtio::device::VirtioBlock;
use super::console::virtio::device::VirtioConsole;
use super::fs::vhost_user::device::VhostUserFs;
use super::mmio::MmioConfig;
use super::mmio::VIRTIO_MMIO_INT_VRING;
use super::mmio::VIRTIO_MMIO_QUEUE_NOTIFY_OFFSET;
use super::net::vhost::device::VhostNet;
use super::net::virtio::device::VirtioNet;
use super::vsock::vhost::device::VhostVsockDevice;
use super::vsock::vhost_user::device::VhostUserVsock;
use api::defines::BAO_IOEVENTFD_FLAG_DATAMATCH;
use api::device_model::BaoDeviceModel;
use api::error::{Error, Result};
use api::types::DeviceConfig;
use event_manager::{EventManager, MutEventSubscriber};
use libc::{MAP_SHARED, PROT_READ, PROT_WRITE};
use std::fmt::{self, Debug};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::io::{FromRawFd, RawFd};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use vhost_user_frontend::{GuestMemoryMmap, GuestRegionMmap};
use virtio_device::VirtioConfig;
use virtio_queue::{Queue, QueueT};
use vm_device::device_manager::IoManager;
use vm_memory::{guest_memory::FileOffset, GuestAddress, MmapRegion};
use vmm_sys_util::eventfd::{EventFd, EFD_NONBLOCK};

use virtio_bindings::virtio_config::{VIRTIO_F_IOMMU_PLATFORM, VIRTIO_F_VERSION_1};

/// Type alias for the subscriber.
pub type Subscriber = Arc<Mutex<dyn MutEventSubscriber + Send>>;

// Clippy thinks that values of the enum are too different in size.
#[allow(clippy::large_enum_variant)]
/// Virtio device type abstraction to pack all possible devices into one enum.
pub enum VirtioDeviceType {
    VirtioBlock(Arc<Mutex<VirtioBlock>>),
    VhostUserFs(Arc<Mutex<VhostUserFs>>),
    VhostVsock(Arc<Mutex<VhostVsockDevice>>),
    VhostNet(Arc<Mutex<VhostNet>>),
    VhostUserVsock(Arc<Mutex<VhostUserVsock>>),
    VirtioNet(Arc<Mutex<VirtioNet>>),
    VirtioConsole(Arc<Mutex<VirtioConsole>>),
    Unknown,
}

/// VirtioDeviceCommon struct.
///
/// # Attributes
///
/// * `config` - The common virtio configuration.
/// * `mmio` - The MMIO configuration.
/// * `irqfd` - The interrupt file descriptor.
/// * `device_model` - The device model.
/// * `regions` - The memory regions of the device.
pub struct VirtioDeviceCommon {
    pub config: VirtioConfig<Queue>,
    pub mmio: MmioConfig,
    pub irqfd: EventFd,
    pub device_model: Arc<Mutex<BaoDeviceModel>>,
    pub regions: Vec<GuestRegionMmap>,
}

impl VirtioDeviceCommon {
    /// Create a new device.
    ///
    /// # Arguments
    ///
    /// * `config` - The device configuration.
    /// * `device_manager` - The device manager.
    /// * `device_model` - The device model.
    ///
    /// # Returns
    ///
    /// A `Result` containing the new device.
    pub fn new(
        config: &DeviceConfig,
        device_model: Arc<Mutex<BaoDeviceModel>>,
        virtio: VirtioConfig<Queue>,
    ) -> Result<Self> {
        // Create the MMIO configuration.
        let mmio = MmioConfig::new(
            config.mmio_addr,
            0x200,
            device_model.lock().unwrap().clone().irq,
        )
        .unwrap();

        // Create a new EventFd for the interrupt (irqfd).
        let irqfd = EventFd::new(0).unwrap();

        // Extract the device model fields.
        let file =
            unsafe { File::from_raw_fd(device_model.lock().unwrap().clone().devmodel_fd as RawFd) };
        let shemem_addr = device_model.lock().unwrap().clone().shmem_addr;
        let shemem_size = device_model.lock().unwrap().clone().shmem_size;

        // Create the device object.
        let mut device = VirtioDeviceCommon {
            config: virtio,
            mmio,
            irqfd: irqfd,
            device_model,
            regions: Vec::new(),
        };

        // Map the region.
        // The mmap_offset is set to 0 because the base address of Bao's shared memory driver is
        // already defined statically in the backend device tree.
        device
            .map_region(0, file, shemem_addr, shemem_size as usize)
            .unwrap();

        // Register the Irqfd (Host to Guest notification).
        device
            .device_model
            .lock()
            .unwrap()
            .register_irqfd(&device.irqfd)
            .unwrap();

        // Return the device object.
        Ok(device)
    }

    /// Perform common initial steps for device activation based on the configuration
    /// like setting up the event file descriptors for guest to host notifications.
    ///
    /// # Returns
    ///
    /// A `Result` containing the event file descriptors.
    pub fn prepare_activate(&self) -> Result<Vec<EventFd>> {
        // Check if the device has already been activated.
        if self.config.device_activated {
            return Err(Error::DeviceAlreadyActivated);
        }

        // We do not support legacy drivers.
        if self.config.driver_features & (1 << VIRTIO_F_VERSION_1) == 0 {
            return Err(Error::DeviceBadFeatures(self.config.driver_features));
        }

        // Create an empty vector to store all event file descriptors.
        let mut ioevents = Vec::new();

        // Right now, we operate under the assumption all queues are marked ready by the device
        // (which is true until we start supporting devices that can optionally make use of
        // additional queues on top of the defaults).
        for (i, _queue) in self.config.queues.iter().enumerate() {
            // Create a new EventFd for the queue (Ioeventfd -> Guest to Host notification).
            let fd = EventFd::new(EFD_NONBLOCK).unwrap();

            // Register the queue event fd.
            self.device_model
                .lock()
                .unwrap()
                .register_ioeventfd(
                    fd.as_raw_fd() as u32,
                    BAO_IOEVENTFD_FLAG_DATAMATCH,
                    self.mmio.range.base().0 + VIRTIO_MMIO_QUEUE_NOTIFY_OFFSET,
                    // The maximum number of queues should fit within an `u16` according to the
                    // standard, so the conversion below is always expected to succeed.
                    i as u64,
                )
                .unwrap();

            ioevents.push(fd);
        }

        Ok(ioevents)
    }

    /// Method to map a region.
    ///
    /// # Arguments
    ///
    /// * `mmap_offset` - Offset of the mmap region.
    /// * `file` - File descriptor of the region.
    /// * `base_addr` - Base address of the region.
    /// * `size` - Size of the region.
    ///
    /// # Returns
    ///
    /// * `Result<()>` - A Result containing Ok(()) on success, or an Error on failure.
    fn map_region(
        &mut self,
        mmap_offset: u64,
        file: File,
        base_addr: u64,
        size: usize,
    ) -> Result<()> {
        // Create a mmap region with proper permissions.
        let mmap_region = match MmapRegion::build(
            Some(FileOffset::new(file, mmap_offset)),
            size as usize,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
        ) {
            Ok(mmap_region) => mmap_region,
            Err(_) => {
                return Err(Error::MmapGuestMemoryFailed);
            }
        };

        // Create a guest region mmap.
        let guest_region_mmap = match GuestRegionMmap::new(mmap_region, GuestAddress(base_addr)) {
            Ok(guest_region_mmap) => guest_region_mmap,
            Err(_) => {
                return Err(Error::MmapGuestMemoryFailed);
            }
        };

        // Push the region to the regions vector.
        // For now, we only have one region since this function is called only once.
        // However, in the future, we may have to support more than one region.
        self.regions.push(guest_region_mmap);

        // Return the guest region mmap.
        Ok(())
    }

    /// Method to get the memory of the device.
    ///
    /// # Returns
    ///
    /// * `GuestMemoryMmap` - Guest memory mmap.
    pub fn mem(&mut self) -> GuestMemoryMmap {
        // Create a new GuestMemoryMmap from the regions without removing them.
        GuestMemoryMmap::from_regions(self.regions.drain(..).collect()).unwrap()
    }
}

/// Trait to model the common virtio device operations.
/// Each virtio device type should implement this trait.
pub trait VirtioDeviceT {
    /// Initialize the generic device.
    ///
    /// # Arguments
    ///
    /// * `config` - The device configuration.
    ///
    /// # Returns
    ///
    /// A `Result` containing the generic device features and the queues.
    fn initialize(config: &DeviceConfig) -> Result<(u64, Vec<Queue>)> {
        // Extract the device type.
        let device_type = VirtioDevType::from(config.device_type.as_str());

        // Extract the number of queues and queue size for the device type.
        let (queue_num, queue_size) = device_type.queue_num_and_size();

        // Create the queues.
        let mut queues = Vec::with_capacity(queue_num);
        for _ in 0..queue_num {
            queues.push(Queue::new(queue_size as u16));
        }

        // Convert the vector of Result<Queue, virtio_queue::Error> to a vector of Queue.
        let queues_converted: Vec<Queue> = queues
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        // Define the generic device features.
        let device_features = 1 << VIRTIO_F_VERSION_1 | 1 << VIRTIO_F_IOMMU_PLATFORM;

        Ok((device_features, queues_converted))
    }

    /// Create a new device.
    ///
    /// # Arguments
    ///
    /// * `config` - The device configuration.
    /// * `device_manager` - The device manager.
    /// * `event_manager` - The event manager.
    /// * `device_model` - The device model.
    ///
    /// # Returns
    ///
    /// A `Result` containing the new device.
    fn new(
        config: &DeviceConfig,
        device_manager: Arc<Mutex<IoManager>>,
        event_manager: Option<Arc<Mutex<EventManager<Arc<Mutex<dyn MutEventSubscriber + Send>>>>>>,
        device_model: Arc<Mutex<BaoDeviceModel>>,
    ) -> Result<Arc<Mutex<Self>>>;

    /// Returns the specific device features.
    ///
    /// # Arguments
    ///
    /// * `config` - The device configuration.
    ///
    /// # Returns
    ///
    /// A `Result` containing the specific device features.
    fn device_features(config: &DeviceConfig) -> Result<u64>;

    /// Returns the specific device configuration space.
    ///
    /// # Arguments
    ///
    /// * `config` - The device configuration.
    ///
    /// # Returns
    ///
    /// A `Result` containing the specific device configuration space.
    fn config_space(config: &DeviceConfig) -> Result<Vec<u8>>;
}

/// Simple trait to model the operation of signalling the driver about used events
/// for the specified queue.
pub trait SignalUsedQueue {
    /// Signals the driver about used events for the specified queue.
    fn signal_used_queue(&self, index: u16);
}

/// Uses a single irqfd as the basis of signalling any queue (useful for the MMIO transport,
/// where a single interrupt is shared for everything).
///
/// # Attributes
///
/// * `irqfd` - The EventFd to be used for signalling.
/// * `interrupt_status` - The interrupt status to be used for signalling.
pub struct SingleFdSignalQueue {
    pub irqfd: EventFd,
    pub interrupt_status: Arc<AtomicU8>,
}

impl SignalUsedQueue for SingleFdSignalQueue {
    /// Signals the driver about used events for the specified queue.
    fn signal_used_queue(&self, _index: u16) {
        // Set the interrupt status.
        self.interrupt_status
            .fetch_or(VIRTIO_MMIO_INT_VRING, Ordering::SeqCst);

        // Write to the eventfd to signal the queue.
        self.irqfd
            .write(1)
            .expect("Failed write to eventfd when signalling queue");
    }
}

/// Virtio types taken from linux/virtio_ids.h
#[derive(Copy, Clone, Debug)]
#[allow(dead_code)]
#[allow(non_camel_case_types)]
#[repr(C)]
pub enum VirtioDevType {
    Net = 1,
    Block = 2,
    Console = 3,
    Rng = 4,
    Balloon = 5,
    Fs9P = 9,
    Gpu = 16,
    Input = 18,
    Vsock = 19,
    Iommu = 23,
    Mem = 24,
    Fs = 26,
    Pmem = 27,
    I2c = 34,
    Watchdog = 35, // Temporary until official number allocated
    Gpio = 41,
    Unknown = 0xFF,
}

impl From<u32> for VirtioDevType {
    fn from(t: u32) -> Self {
        match t {
            1 => VirtioDevType::Net,
            2 => VirtioDevType::Block,
            3 => VirtioDevType::Console,
            4 => VirtioDevType::Rng,
            5 => VirtioDevType::Balloon,
            9 => VirtioDevType::Fs9P,
            16 => VirtioDevType::Gpu,
            18 => VirtioDevType::Input,
            19 => VirtioDevType::Vsock,
            23 => VirtioDevType::Iommu,
            24 => VirtioDevType::Mem,
            26 => VirtioDevType::Fs,
            27 => VirtioDevType::Pmem,
            34 => VirtioDevType::I2c,
            35 => VirtioDevType::Watchdog,
            41 => VirtioDevType::Gpio,
            _ => VirtioDevType::Unknown,
        }
    }
}

impl From<&str> for VirtioDevType {
    fn from(t: &str) -> Self {
        match t {
            "net" => VirtioDevType::Net,
            "block" => VirtioDevType::Block,
            "console" => VirtioDevType::Console,
            "rng" => VirtioDevType::Rng,
            "balloon" => VirtioDevType::Balloon,
            "fs9p" => VirtioDevType::Fs9P,
            "gpu" => VirtioDevType::Gpu,
            "input" => VirtioDevType::Input,
            "vsock" => VirtioDevType::Vsock,
            "iommu" => VirtioDevType::Iommu,
            "mem" => VirtioDevType::Mem,
            "fs" => VirtioDevType::Fs,
            "pmem" => VirtioDevType::Pmem,
            "i2c" => VirtioDevType::I2c,
            "watchdog" => VirtioDevType::Watchdog,
            "gpio" => VirtioDevType::Gpio,
            _ => VirtioDevType::Unknown,
        }
    }
}

impl From<VirtioDevType> for String {
    fn from(t: VirtioDevType) -> String {
        match t {
            VirtioDevType::Net => "net",
            VirtioDevType::Block => "block",
            VirtioDevType::Console => "console",
            VirtioDevType::Rng => "rng",
            VirtioDevType::Balloon => "balloon",
            VirtioDevType::Gpu => "gpu",
            VirtioDevType::Fs9P => "9p",
            VirtioDevType::Input => "input",
            VirtioDevType::Vsock => "vsock",
            VirtioDevType::Iommu => "iommu",
            VirtioDevType::Mem => "mem",
            VirtioDevType::Fs => "fs",
            VirtioDevType::Pmem => "pmem",
            VirtioDevType::I2c => "i2c",
            VirtioDevType::Watchdog => "watchdog",
            VirtioDevType::Gpio => "gpio",
            VirtioDevType::Unknown => "UNKNOWN",
        }
        .to_string()
    }
}

// In order to use the `{}` marker, the trait `fmt::Display` must be implemented
// manually for the type VirtioDevType.
impl fmt::Display for VirtioDevType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}",
            match *self {
                VirtioDevType::Net => String::from("Net"),
                VirtioDevType::Block => String::from("Block"),
                VirtioDevType::Console => String::from("Console"),
                VirtioDevType::Rng => String::from("Rng"),
                VirtioDevType::Balloon => String::from("Balloon"),
                VirtioDevType::Gpu => String::from("Gpu"),
                VirtioDevType::Fs9P => String::from("Fs9P"),
                VirtioDevType::Input => String::from("Input"),
                VirtioDevType::Vsock => String::from("Vsock"),
                VirtioDevType::Iommu => String::from("Iommu"),
                VirtioDevType::Mem => String::from("Mem"),
                VirtioDevType::Fs => String::from("Fs"),
                VirtioDevType::Pmem => String::from("Pmem"),
                VirtioDevType::I2c => String::from("I2c"),
                VirtioDevType::Watchdog => String::from("Watchdog"),
                VirtioDevType::Gpio => String::from("Gpio"),
                VirtioDevType::Unknown => String::from("Unknown"),
            }
        )
    }
}

impl VirtioDevType {
    /// Returns the number of queues and the queue size for the device type.
    pub fn queue_num_and_size(&self) -> (usize, usize) {
        match *self {
            VirtioDevType::Net => (2, 1024),
            VirtioDevType::Block => (1, 256),
            VirtioDevType::Console => (2, 1024),
            VirtioDevType::Rng => (1, 1024),
            VirtioDevType::Balloon => (0, 0),
            VirtioDevType::Gpu => (0, 0),
            VirtioDevType::Fs9P => (0, 0),
            VirtioDevType::Input => (0, 0),
            VirtioDevType::Vsock => (3, 1024), // Virtio spec says 3 queues, but vhost/vhost-user only manage 2 (RX/TX)
            VirtioDevType::Mem => (0, 0),
            VirtioDevType::Fs => (2, 1024),
            VirtioDevType::Pmem => (0, 0),
            VirtioDevType::I2c => (1, 1024),
            VirtioDevType::Watchdog => (0, 0),
            VirtioDevType::Gpio => (2, 256),
            _ => (0, 0),
        }
    }
}

/// Virtio data plane types.
#[derive(Copy, Clone, Debug)]
#[allow(dead_code)]
#[allow(non_camel_case_types)]
pub enum VirtioDataPlane {
    Virtio = 0,
    Vhost = 1,
    VhostUser = 2,
    Unknown = 0xFF,
}

impl From<u32> for VirtioDataPlane {
    fn from(t: u32) -> Self {
        match t {
            0 => VirtioDataPlane::Virtio,
            1 => VirtioDataPlane::Vhost,
            2 => VirtioDataPlane::VhostUser,
            _ => VirtioDataPlane::Unknown,
        }
    }
}

impl From<&str> for VirtioDataPlane {
    fn from(t: &str) -> Self {
        match t {
            "virtio" => VirtioDataPlane::Virtio,
            "vhost" => VirtioDataPlane::Vhost,
            "vhost_user" => VirtioDataPlane::VhostUser,
            _ => VirtioDataPlane::Unknown,
        }
    }
}

impl From<VirtioDataPlane> for String {
    fn from(t: VirtioDataPlane) -> String {
        match t {
            VirtioDataPlane::Virtio => "virtio",
            VirtioDataPlane::Vhost => "vhost",
            VirtioDataPlane::VhostUser => "vhost_user",
            VirtioDataPlane::Unknown => "UNKNOWN",
        }
        .to_string()
    }
}

// In order to use the `{}` marker, the trait `fmt::Display` must be implemented
// manually for the type VirtioDataPlane.
impl fmt::Display for VirtioDataPlane {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{}",
            match *self {
                VirtioDataPlane::Virtio => String::from("Virtio"),
                VirtioDataPlane::Vhost => String::from("Vhost"),
                VirtioDataPlane::VhostUser => String::from("VhostUser"),
                VirtioDataPlane::Unknown => String::from("Unknown"),
            }
        )
    }
}

/// Helper for cloning a Queue since QueueState doesn't derive Clone
pub fn clone_queue(queue: &Queue) -> Queue {
    let mut q = Queue::new(queue.max_size()).unwrap();

    q.set_next_avail(queue.next_avail());
    q.set_next_used(queue.next_used());
    q.set_event_idx(queue.event_idx_enabled());
    q.set_size(queue.size());
    q.set_ready(queue.ready());
    q.try_set_desc_table_address(GuestAddress(queue.desc_table()))
        .unwrap();
    q.try_set_avail_ring_address(GuestAddress(queue.avail_ring()))
        .unwrap();
    q.try_set_used_ring_address(GuestAddress(queue.used_ring()))
        .unwrap();

    q
}
