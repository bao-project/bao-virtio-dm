use super::bindings;
use super::queue_handler::QueueHandler;
use super::simple_handler::SimpleHandler;
use super::tap::Tap;
use crate::device::clone_queue;
use crate::device::{SingleFdSignalQueue, Subscriber, VirtioDeviceT};
use crate::device::{VirtioDevType, VirtioDeviceCommon};
use crate::net::utils::mac_address_to_bytes;
use crate::net::virtio::VIRTIO_NET_HDR_SIZE;
use api::device_model::BaoDeviceModel;
use api::error::{Error, Result};
use api::types::DeviceConfig;
use event_manager::{
    EventManager, MutEventSubscriber, RemoteEndpoint, Result as EvmgrResult, SubscriberId,
};
use std::borrow::{Borrow, BorrowMut};
use std::sync::{Arc, Mutex};
use virtio_bindings::virtio_config::VIRTIO_F_IN_ORDER;
use virtio_bindings::virtio_net::{
    VIRTIO_NET_F_CSUM, VIRTIO_NET_F_GUEST_CSUM, VIRTIO_NET_F_GUEST_TSO4, VIRTIO_NET_F_GUEST_TSO6,
    VIRTIO_NET_F_GUEST_UFO, VIRTIO_NET_F_HOST_TSO4, VIRTIO_NET_F_HOST_TSO6, VIRTIO_NET_F_HOST_UFO,
    VIRTIO_NET_F_MAC, VIRTIO_NET_F_MRG_RXBUF,
};
use virtio_device::{VirtioConfig, VirtioDeviceActions, VirtioDeviceType, VirtioMmioDevice};
use virtio_queue::Queue;
use vm_device::bus::MmioAddress;
use vm_device::device_manager::{IoManager, MmioManager};
use vm_device::MutDeviceMmio;

const VIRTIO_F_RING_EVENT_IDX: u64 = 29;

/// Virtio net device.
///
/// # Attributes
///
/// * `common` - Virtio common device.
/// * `endpoint` - The remote subscriber endpoint.
/// * `tap_name` - Name of the tap device.
pub struct VirtioNet {
    pub common: VirtioDeviceCommon,
    pub endpoint: RemoteEndpoint<Subscriber>,
    pub tap_name: String,
}

impl VirtioDeviceT for VirtioNet {
    fn new(
        config: &DeviceConfig,
        device_manager: Arc<Mutex<IoManager>>,
        event_manager: Option<Arc<Mutex<EventManager<Arc<Mutex<dyn MutEventSubscriber + Send>>>>>>,
        device_model: Arc<Mutex<BaoDeviceModel>>,
    ) -> Result<Arc<Mutex<Self>>> {
        // Extract the generic features and queues.
        let (common_features, queues) = Self::initialize(&config).unwrap();

        // Update the device features.
        let device_features = common_features | Self::device_features(&config).unwrap();

        // Update the configuration space.
        let config_space = Self::config_space(&config).unwrap();

        // Create a VirtioConfig object.
        let virtio_cfg = VirtioConfig::new(device_features, queues, config_space);

        // Create the generic device.
        let common_device = VirtioDeviceCommon::new(config, device_model, virtio_cfg).unwrap();

        // Create a remote endpoint object, that allows interacting with the VM EventManager from a different thread.
        let remote_endpoint = event_manager.unwrap().lock().unwrap().remote_endpoint();

        // Create the net device.
        let net = Arc::new(Mutex::new(VirtioNet {
            common: common_device,
            endpoint: remote_endpoint,
            tap_name: config.tap_name.clone().unwrap(),
        }));

        // Register the MMIO device within the device manager with the specified range.
        device_manager
            .lock()
            .unwrap()
            .register_mmio(net.clone().lock().unwrap().common.mmio.range, net.clone())
            .unwrap();

        // Return the net device.
        Ok(net)
    }

    fn device_features(config: &DeviceConfig) -> Result<u64> {
        let mut features = (1 << VIRTIO_F_RING_EVENT_IDX)
            | (1 << VIRTIO_F_IN_ORDER)
            | (1 << VIRTIO_NET_F_CSUM)
            | (1 << VIRTIO_NET_F_GUEST_CSUM)
            | (1 << VIRTIO_NET_F_MRG_RXBUF)
            | (1 << VIRTIO_NET_F_GUEST_TSO4)
            | (1 << VIRTIO_NET_F_GUEST_TSO6)
            | (1 << VIRTIO_NET_F_GUEST_UFO)
            | (1 << VIRTIO_NET_F_HOST_TSO4)
            | (1 << VIRTIO_NET_F_HOST_TSO6)
            | (1 << VIRTIO_NET_F_HOST_UFO);

        // Set the mac address feature if a mac address is provided.
        if config.mac_addr.is_some() {
            features |= 1 << VIRTIO_NET_F_MAC;
        }

        Ok(features)
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

impl Borrow<VirtioConfig<Queue>> for VirtioNet {
    fn borrow(&self) -> &VirtioConfig<Queue> {
        &self.common.config
    }
}

impl BorrowMut<VirtioConfig<Queue>> for VirtioNet {
    fn borrow_mut(&mut self) -> &mut VirtioConfig<Queue> {
        &mut self.common.config
    }
}

impl VirtioDeviceType for VirtioNet {
    fn device_type(&self) -> u32 {
        VirtioDevType::Net as u32
    }
}

/// Implement the `VirtioDeviceActions` trait to add our custom device actions.
impl VirtioDeviceActions for VirtioNet {
    type E = Error;

    fn activate(&mut self) -> Result<()> {
        // Create the tap device.
        let tap = Tap::open_named(self.tap_name.as_str())?;

        // driver_features is already populated by activate() time
        let df = self.common.config.driver_features;
        let mut flags = 0;
        if df & (1 << VIRTIO_NET_F_GUEST_CSUM) != 0 {
            flags |= bindings::TUN_F_CSUM;
        }
        if df & (1 << VIRTIO_NET_F_GUEST_TSO4) != 0 {
            flags |= bindings::TUN_F_TSO4;
        }
        if df & (1 << VIRTIO_NET_F_GUEST_TSO6) != 0 {
            flags |= bindings::TUN_F_TSO6;
        }
        if df & (1 << VIRTIO_NET_F_GUEST_UFO) != 0 {
            flags |= bindings::TUN_F_UFO;
        }
        tap.set_offload(flags)?; // 0 for a minimal guest, full set for Linux

        // The layout of the header is specified in the standard and is 12 bytes in size. We
        // should define this somewhere.
        tap.set_vnet_hdr_size(VIRTIO_NET_HDR_SIZE as i32)?;

        // Create the driver notify object.
        let driver_notify = SingleFdSignalQueue {
            irqfd: self.common.irqfd.try_clone().unwrap(),
            interrupt_status: self.common.config.interrupt_status.clone(),
        };

        // Prepare the activation by calling the generic `prepare_activate` method.
        let mut ioevents = self.common.prepare_activate()?;

        // Create the inner handler.
        let rxq = clone_queue(&self.common.config.queues[0]);
        let txq = clone_queue(&self.common.config.queues[1]);
        let mergeable = df & (1 << VIRTIO_NET_F_MRG_RXBUF) != 0;
        let inner = SimpleHandler::new(driver_notify, rxq, txq, tap, self.common.mem(), mergeable);

        // Create the queue handler.
        let handler = Arc::new(Mutex::new(QueueHandler {
            inner,
            rx_ioevent: ioevents.remove(0),
            tx_ioevent: ioevents.remove(0),
        }));

        // Register the queue handler with the `EventManager`. We could record the `sub_id`
        // (and/or keep a handler clone) for further interaction (i.e. to remove the subscriber at
        // a later time, retrieve state, etc).
        let _sub_id = self
            .endpoint
            .call_blocking(move |mgr| -> EvmgrResult<SubscriberId> {
                Ok(mgr.add_subscriber(handler))
            })
            .unwrap();

        // Set the device as activated.
        self.common.config.device_activated = true;

        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        // Not implemented for now.
        Ok(())
    }
}

/// Implement the `VirtioMmioDevice` trait to add VirtIO MMIO support to our device.
impl VirtioMmioDevice for VirtioNet {
    fn queue_notify(&mut self, _val: u32) {
        // Do nothing for now.
    }
}

/// Implement the `DeviceMmio` mutable trait to add MMIO support to our device.
/// Otherwise we could not register the device within the device manager.
impl MutDeviceMmio for VirtioNet {
    fn mmio_read(&mut self, _base: MmioAddress, offset: u64, data: &mut [u8]) {
        self.read(offset, data);
    }

    fn mmio_write(&mut self, _base: MmioAddress, offset: u64, data: &[u8]) {
        self.write(offset, data);
    }
}
