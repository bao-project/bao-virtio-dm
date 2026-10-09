//! Virtio input device fed through a uinput sink in the backend VM.
//!
//! The device creates a uinput device in the backend (a keyboard and a mouse: keys, buttons
//! and relative axes) and forwards whatever is written into its evdev node to the frontend,
//! which sees a keyboard and a mouse behind one virtio-input device. The node is grabbed, so
//! the backend's own input stack does not act on what is written. What goes in, and from
//! where, is up to the backend: forwarded from its own devices, synthesized, anything that
//! writes input events. An alias from the configuration gives the node a fixed path, as the
//! console's pty alias does.

use super::evdev::{self, Bitmap, EV_KEY, EV_MAX, EV_REL, KEY_MAX, REL_MAX};
use super::input_handler::InputHandler;
use crate::device::{SingleFdSignalQueue, Subscriber, VirtioDeviceT};
use crate::device::{VirtioDevType, VirtioDeviceCommon};
use api::device_model::BaoDeviceModel;
use api::error::{Error, Result};
use api::types::DeviceConfig;
use log::info;
use event_manager::{
    EventManager, MutEventSubscriber, RemoteEndpoint, Result as EvmgrResult, SubscriberId,
};
use std::borrow::{Borrow, BorrowMut};
use std::sync::{Arc, Mutex};
use virtio_device::{VirtioConfig, VirtioDeviceActions, VirtioDeviceType, VirtioMmioDevice};
use virtio_queue::Queue;
use vm_device::bus::MmioAddress;
use vm_device::device_manager::{IoManager, MmioManager};
use vm_device::MutDeviceMmio;

/// struct virtio_input_config: select, subsel, size, reserved[5], then a 128-byte union
const CONFIG_LEN: usize = 8 + 128;
const CFG_UNSET: u8 = 0x00;
const CFG_ID_NAME: u8 = 0x01;
const CFG_ID_SERIAL: u8 = 0x02;
const CFG_ID_DEVIDS: u8 = 0x03;
const CFG_PROP_BITS: u8 = 0x10;
const CFG_EV_BITS: u8 = 0x11;
const CFG_ABS_INFO: u8 = 0x12;

const DEVICE_NAME: &str = "Bao shared keyboard and mouse";

pub struct VirtioInput {
    pub common: VirtioDeviceCommon,
    pub endpoint: RemoteEndpoint<Subscriber>,
    pub config: DeviceConfig,
    ids: [u16; 4],
    key_bits: Bitmap,
    rel_bits: Bitmap,
    select: u8,
    subsel: u8,
    sink: evdev::Sink,
}

impl VirtioInput {
    /// The union payload for the selected item, and its size byte
    fn selected(&self) -> (u8, Vec<u8>) {
        match self.select {
            CFG_ID_NAME => {
                let name = DEVICE_NAME.as_bytes();
                (name.len() as u8, name.to_vec())
            }
            CFG_ID_SERIAL => (0, Vec::new()),
            CFG_ID_DEVIDS => {
                let mut ids = Vec::with_capacity(8);
                for id in self.ids.iter() {
                    ids.extend_from_slice(&id.to_le_bytes());
                }
                (8, ids)
            }
            CFG_PROP_BITS => (0, Vec::new()),
            CFG_EV_BITS => {
                let map: Option<(&Bitmap, u16)> = match self.subsel as u16 {
                    0 => None,
                    EV_KEY => Some((&self.key_bits, KEY_MAX)),
                    EV_REL => Some((&self.rel_bits, REL_MAX)),
                    _ => None,
                };
                match map {
                    Some((bits, max)) => {
                        let len = (max as usize + 8) / 8;
                        (len as u8, bits[..len].to_vec())
                    }
                    None if self.subsel as u16 == 0 => {
                        // The event types themselves: EV_KEY and EV_REL (EV_SYN is implied)
                        let mut types = vec![0u8; (EV_MAX as usize + 8) / 8];
                        types[0] |= 1 << EV_KEY;
                        types[0] |= 1 << EV_REL;
                        (types.len() as u8, types)
                    }
                    None => (0, Vec::new()),
                }
            }
            CFG_ABS_INFO => (0, Vec::new()),
            _ => (0, Vec::new()),
        }
    }

    /// Rewrites the config space for the current select and subsel
    fn fill_config(&mut self) {
        let (size, payload) = self.selected();
        let space = &mut self.common.config.config_space;
        space.clear();
        space.resize(CONFIG_LEN, 0);
        space[0] = self.select;
        space[1] = self.subsel;
        space[2] = size;
        let n = payload.len().min(128);
        space[8..8 + n].copy_from_slice(&payload[..n]);
    }
}

impl VirtioDeviceT for VirtioInput {
    fn new(
        config: &DeviceConfig,
        device_manager: Arc<Mutex<IoManager>>,
        event_manager: Option<Arc<Mutex<EventManager<Arc<Mutex<dyn MutEventSubscriber + Send>>>>>>,
        device_model: Arc<Mutex<BaoDeviceModel>>,
    ) -> Result<Arc<Mutex<Self>>> {
        let (common_features, queues) = Self::initialize(&config).unwrap();
        let device_features = common_features | Self::device_features(&config).unwrap();
        let config_space = Self::config_space(&config).unwrap();
        let virtio_cfg = VirtioConfig::new(device_features, queues, config_space);
        let common_device = VirtioDeviceCommon::new(config, device_model, virtio_cfg).unwrap();
        let remote_endpoint = event_manager.unwrap().lock().unwrap().remote_endpoint();

        // A full keyboard and a mouse
        let mut key_bits: Bitmap = [0; 128];
        let mut rel_bits: Bitmap = [0; 128];
        for code in 1..=evdev::KEY_KEYBOARD_MAX {
            key_bits[code as usize / 8] |= 1 << (code % 8);
        }
        for code in evdev::BTN_MOUSE..=evdev::BTN_TASK {
            key_bits[code as usize / 8] |= 1 << (code % 8);
        }
        for code in [evdev::REL_X, evdev::REL_Y, evdev::REL_HWHEEL, evdev::REL_WHEEL] {
            rel_bits[code as usize / 8] |= 1 << (code % 8);
        }
        let ids = [evdev::BUS_VIRTUAL, 0x1af4, 0x0012, 0x0001];

        let sink_name = format!("{}{}", DEVICE_NAME, config.id);
        let sink = evdev::Sink::create(&sink_name, ids, &key_bits, &rel_bits)
            .unwrap_or_else(|e| panic!("virtio-input: uinput sink: {}", e));
        let sink_path = match config.evdev_alias.clone() {
            Some(alias) => {
                let _ = std::fs::remove_file(&alias);
                std::os::unix::fs::symlink(&sink.path, &alias)
                    .unwrap_or_else(|e| panic!("virtio-input: alias {}: {}", alias, e));
                alias
            }
            None => sink.path.clone(),
        };
        info!("virtio-input device id {} at {}", config.id, sink_path);

        let input = Arc::new(Mutex::new(VirtioInput {
            common: common_device,
            endpoint: remote_endpoint,
            config: config.clone(),
            ids,
            key_bits,
            rel_bits,
            select: CFG_UNSET,
            subsel: 0,
            sink,
        }));
        input.lock().unwrap().fill_config();

        device_manager
            .lock()
            .unwrap()
            .register_mmio(input.clone().lock().unwrap().common.mmio.range, input.clone())
            .unwrap();

        Ok(input)
    }

    fn device_features(_config: &DeviceConfig) -> Result<u64> {
        Ok(0)
    }

    fn config_space(_config: &DeviceConfig) -> Result<Vec<u8>> {
        Ok(vec![0u8; CONFIG_LEN])
    }
}

impl Borrow<VirtioConfig<Queue>> for VirtioInput {
    fn borrow(&self) -> &VirtioConfig<Queue> {
        &self.common.config
    }
}

impl BorrowMut<VirtioConfig<Queue>> for VirtioInput {
    fn borrow_mut(&mut self) -> &mut VirtioConfig<Queue> {
        &mut self.common.config
    }
}

impl VirtioDeviceType for VirtioInput {
    fn device_type(&self) -> u32 {
        VirtioDevType::Input as u32
    }
}

impl VirtioDeviceActions for VirtioInput {
    type E = Error;

    fn activate(&mut self) -> Result<()> {
        let driver_notify = SingleFdSignalQueue {
            irqfd: self.common.irqfd.try_clone().unwrap(),
            interrupt_status: self.common.config.interrupt_status.clone(),
        };

        let mut ioevents = self.common.prepare_activate().unwrap();

        let mut source = evdev::Source::open(&self.sink.path)
            .unwrap_or_else(|e| panic!("virtio-input: {}: {}", self.sink.path, e));
        source
            .grab()
            .unwrap_or_else(|e| panic!("virtio-input: grab {}: {}", self.sink.path, e));

        let handler = Arc::new(Mutex::new(InputHandler::new(
            driver_notify,
            self.common.mem(),
            self.common.config.queues.remove(0),
            self.common.config.queues.remove(0),
            ioevents.remove(0),
            ioevents.remove(0),
            source,
        )));

        let _sub_id = self
            .endpoint
            .call_blocking(move |mgr| -> EvmgrResult<SubscriberId> {
                Ok(mgr.add_subscriber(handler))
            })
            .unwrap();

        self.common.config.device_activated = true;

        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        Ok(())
    }

    /// The driver selects what it wants to read next; the rest of the space is read-only
    fn write_config(&mut self, offset: usize, data: &[u8]) {
        for (i, byte) in data.iter().enumerate() {
            match offset + i {
                0 => self.select = *byte,
                1 => self.subsel = *byte,
                _ => {}
            }
        }
        self.fill_config();
    }
}

impl VirtioMmioDevice for VirtioInput {
    fn queue_notify(&mut self, _val: u32) {
        // The queues are served through the ioeventfds
    }
}

impl MutDeviceMmio for VirtioInput {
    fn mmio_read(&mut self, _base: MmioAddress, offset: u64, data: &mut [u8]) {
        self.read(offset, data);
    }

    fn mmio_write(&mut self, _base: MmioAddress, offset: u64, data: &[u8]) {
        self.write(offset, data);
    }
}
