use crate::device::clone_queue;
use crate::device::VirtioDeviceT;
use crate::device::{VirtioDevType, VirtioDeviceCommon};
use crate::mmio::VIRTIO_MMIO_INT_VRING;
use crate::net::utils::mac_address_to_bytes;
use crate::net::virtio::bindings;
use crate::net::virtio::tap::Tap;
use crate::net::virtio::VIRTIO_NET_HDR_SIZE;
use crate::vhost::{VhostKernelCommon, VHOST_FEATURES};
use api::device_model::BaoDeviceModel;
use api::error::{Error, Result};
use api::types::DeviceConfig;
use event_manager::{EventManager, MutEventSubscriber};
use std::borrow::{Borrow, BorrowMut};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use vhost::net::VhostNet as VhostNetBackend;
use vhost::vhost_kern::net::Net;
use vhost::vhost_kern::VhostKernBackend;
use vhost::{VhostBackend, VringConfigData};
use vhost_user_frontend::GuestMemoryMmap;
use virtio_bindings::virtio_config::{VIRTIO_F_NOTIFY_ON_EMPTY, VIRTIO_F_RING_RESET};
use virtio_bindings::virtio_net::VIRTIO_NET_F_MRG_RXBUF;
use virtio_device::{VirtioConfig, VirtioDeviceActions, VirtioDeviceType, VirtioMmioDevice};
use virtio_queue::{Queue, QueueT};
use vm_device::bus::MmioAddress;
use vm_device::device_manager::{IoManager, MmioManager};
use vm_device::MutDeviceMmio;
use vm_memory::GuestAddressSpace;

const VIRTIO_RING_F_INDIRECT_DESC: u32 = 28;
const VIRTIO_F_RING_EVENT_IDX: u64 = 29;

/// Vhost net device.
///
/// # Attributes
///
/// * `virtio` - Virtio virtio device.
/// * `vhost` - Vhost kernel common device.
/// * `net` - Net device.
/// * `tap_name` - Name of the tap device.
pub struct VhostNet {
    pub virtio: VirtioDeviceCommon,
    pub vhost: VhostKernelCommon,
    pub net: Net<Arc<GuestMemoryMmap>>,
    pub tap_name: String,
}

impl VirtioDeviceT for VhostNet {
    fn new(
        config: &DeviceConfig,
        device_manager: Arc<Mutex<IoManager>>,
        _event_manager: Option<Arc<Mutex<EventManager<Arc<Mutex<dyn MutEventSubscriber + Send>>>>>>,
        device_model: Arc<Mutex<BaoDeviceModel>>,
    ) -> Result<Arc<Mutex<Self>>> {
        // Extract the generic features and queues.
        let (common_features, queues) = Self::initialize(&config).unwrap();

        // Extract the device features.
        let device_features = Self::device_features(&config).unwrap();

        // Update the configuration space.
        let config_space = Self::config_space(&config).unwrap();

        // Create a VirtioConfig object.
        let virtio_cfg = VirtioConfig::new(common_features | device_features, queues, config_space);

        // Create the generic device.
        let mut common_device = VirtioDeviceCommon::new(config, device_model, virtio_cfg).unwrap();

        // Extract the VirtioDeviceCommon MMIO range.
        let range = common_device.mmio.range;

        // Create the Net kernel device.
        let net_kernel = Net::new(Arc::new(common_device.mem())).unwrap();

        // Create the net device.
        let net = Arc::new(Mutex::new(VhostNet {
            virtio: common_device,
            vhost: VhostKernelCommon::new(device_features).unwrap(),
            net: net_kernel,
            tap_name: config.tap_name.clone().unwrap(),
        }));

        // Register the MMIO device within the device manager with the specified range.
        device_manager
            .lock()
            .unwrap()
            .register_mmio(range, net.clone())
            .unwrap();

        // Return the net device.
        Ok(net)
    }

    fn device_features(_config: &DeviceConfig) -> Result<u64> {
        let features = (1 << VIRTIO_F_RING_EVENT_IDX)
            | (1 << VIRTIO_F_NOTIFY_ON_EMPTY)
            | (1 << VIRTIO_F_RING_RESET)
            | (1 << VIRTIO_RING_F_INDIRECT_DESC)
            | (1 << VIRTIO_NET_F_MRG_RXBUF);

        Ok(features | VHOST_FEATURES)
    }

    fn config_space(config: &DeviceConfig) -> Result<Vec<u8>> {
        // TODO: Maybe we will need in the future to support setting other fields in the
        // configuration space. For now, we only need the mac address.
        // Info: https://docs.oasis-open.org/virtio/virtio/v1.2/csd01/virtio-v1.2-csd01.html#x1-2230004

        // Extract the mac address.
        let mut mac_addr = Vec::new();
        if config.mac_addr.is_some() {
            mac_addr = mac_address_to_bytes(config.mac_addr.clone().unwrap().as_str()).unwrap();
        }

        // Retrieve the mac address from the device configuration space.
        Ok(mac_addr)
    }
}

impl Borrow<VirtioConfig<Queue>> for VhostNet {
    fn borrow(&self) -> &VirtioConfig<Queue> {
        &self.virtio.config
    }
}

impl BorrowMut<VirtioConfig<Queue>> for VhostNet {
    fn borrow_mut(&mut self) -> &mut VirtioConfig<Queue> {
        &mut self.virtio.config
    }
}

impl VirtioDeviceType for VhostNet {
    fn device_type(&self) -> u32 {
        VirtioDevType::Net as u32
    }
}

/// Implement the `VirtioDeviceActions` trait to add our custom device actions.
impl VirtioDeviceActions for VhostNet {
    type E = Error;

    // This method is called after the driver acknowledges all the device features.
    // For that reasosn, it is the right place to perform the device initialization.
    fn activate(&mut self) -> Result<()> {
        // Create the tap device.
        let tap = Tap::open_named(self.tap_name.as_str())?;

        // Set offload flags to match the relevant virtio features of the device (for now,
        // statically set in the constructor.
        tap.set_offload(
            bindings::TUN_F_CSUM
                | bindings::TUN_F_UFO
                | bindings::TUN_F_TSO4
                | bindings::TUN_F_TSO6,
        )?;

        // The layout of the header is specified in the standard and is 12 bytes in size. We
        // should define this somewhere.
        tap.set_vnet_hdr_size(VIRTIO_NET_HDR_SIZE as i32)?;

        // Setup the ioeventfds by calling the generic `prepare_activate` method.
        let ioevents = self.virtio.prepare_activate().unwrap();

        // Format the queues and ioevents into a Vec<(usize, Queue, EventFd)>.
        let queues = self
            .virtio
            .config
            .queues
            .iter()
            .enumerate()
            .zip(ioevents)
            .map(|((i, queue), ioevent)| (i, clone_queue(&queue), ioevent))
            .collect::<Vec<_>>();

        // Set the current process as the owner of the file descriptor.
        self.net.set_owner().unwrap();

        // Get the device features.
        let supported_backend_features = self.net.get_features().unwrap();

        // The ring layout follows what the driver accepted, not what was offered: a driver may
        // decline mergeable buffers or the event index, and vhost must agree with it
        let driver_features = self.virtio.config.driver_features & supported_backend_features;
        log::info!(
            "virtio-net: offered 0x{:x}, driver accepted 0x{:x}, vhost gets 0x{:x}",
            self.vhost.features(),
            self.virtio.config.driver_features,
            driver_features
        );
        self.net.set_features(driver_features).unwrap();

        // Update the memory table.
        self.net
            .set_mem_table(self.vhost.memory(self.net.mem()).unwrap().as_slice())
            .unwrap();

        // Set the vring.
        let mem = self.net.mem();
        let mem_aux: &GuestMemoryMmap = &mem.memory();

        for (queue_index, queue, ioeventfd) in queues.iter() {
            // Set the vring num.
            self.net.set_vring_num(*queue_index, queue.size()).unwrap();

            let config_data = VringConfigData {
                queue_max_size: queue.max_size(),
                queue_size: queue.size(),
                flags: 0u32,
                desc_table_addr: queue.desc_table(),
                used_ring_addr: queue.used_ring(),
                avail_ring_addr: queue.avail_ring(),
                log_addr: None,
            };

            // Set the vring base.
            self.net
                .set_vring_base(
                    *queue_index,
                    queue.avail_idx(mem_aux, Ordering::Acquire).unwrap().0,
                )
                .unwrap();

            // Set the vring address.
            self.net.set_vring_addr(*queue_index, &config_data).unwrap();

            // Set the vring call.
            self.net
                .set_vring_call(*queue_index, &self.virtio.irqfd.try_clone().unwrap())
                .unwrap();

            // Set the vring kick.
            self.net.set_vring_kick(*queue_index, ioeventfd).unwrap();

            // Set the backend.
            self.net
                .set_backend(*queue_index, Some(&tap.tap_file))
                .unwrap();
        }

        // Set the device as activated.
        self.virtio.config.device_activated = true;

        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        // Not implemented for now.
        Ok(())
    }

    // This method is called when the driver needs to read the interrupt status from the device.
    // Since it's the frontend device responsibility to manage the interrupt status, we need to invoke
    // dedicated logic to update the interrupt status accordingly (Used Buffer Notification or Configuration Change Notification).
    // Note: If the device is implemented in the VMM, the interrupt status can be managed and updated directly by the device.
    fn interrupt_status(&self) -> &Arc<AtomicU8> {
        // We assume that all the interrupts are Used Buffer Notifications.
        self.virtio
            .config
            .interrupt_status
            .fetch_or(VIRTIO_MMIO_INT_VRING, Ordering::SeqCst);
        &self.virtio.config.interrupt_status
    }
}

/// Implement the `VirtioMmioDevice` trait to add VirtIO MMIO support to our device.
impl VirtioMmioDevice for VhostNet {
    fn queue_notify(&mut self, _val: u32) {
        // Do nothing, since the vhost-user backend device is responsible for managing the queue notifications.
        // through Ioeventfds.
    }
}

/// Implement the `DeviceMmio` mutable trait to add MMIO support to our device.
/// Otherwise we could not register the device within the device manager.
impl MutDeviceMmio for VhostNet {
    fn mmio_read(&mut self, _base: MmioAddress, offset: u64, data: &mut [u8]) {
        self.read(offset, data);
    }

    fn mmio_write(&mut self, _base: MmioAddress, offset: u64, data: &[u8]) {
        self.write(offset, data);
    }
}
