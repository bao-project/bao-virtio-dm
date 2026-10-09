use api::defines::{BAO_IO_READ, BAO_IO_WRITE};
use api::device_model::BaoDeviceModel;
use api::error::{Error, Result};
use api::types::DeviceConfig;
use event_manager::{EventManager, MutEventSubscriber};
use std::sync::{Arc, Mutex};
use virtio::block::virtio::device::VirtioBlock;
use virtio::console::virtio::device::VirtioConsole;
use virtio::input::virtio::device::VirtioInput;
use virtio::device::VirtioDeviceT;
use virtio::device::{VirtioDataPlane, VirtioDevType, VirtioDeviceType};
use virtio::fs::vhost_user::device::VhostUserFs;
use virtio::net::vhost::device::VhostNet;
use virtio::net::virtio::device::VirtioNet;
use virtio::vsock::vhost::device::VhostVsockDevice;
use virtio::vsock::vhost_user::device::VhostUserVsock;
use vm_device::bus::MmioAddress;
use vm_device::device_manager::{IoManager, MmioManager};

/// Vm abstraction.
///
/// # Attributes
///
/// * `id` - The ID of the VM.
/// * `device_model` - The device model.
/// * `devices` - The list of devices.
/// * `device_manager` - The device manager responsible for providing methods for device registration, as well as for dispatching read and write requests.
/// * `event_manager` - The event manager responsible for handling and dispatch the device events.
pub struct Vm {
    pub id: u16,
    device_model: Arc<Mutex<BaoDeviceModel>>,
    devices: Vec<VirtioDeviceType>,
    device_manager: Arc<Mutex<IoManager>>,
    pub event_manager: Option<Arc<Mutex<EventManager<Arc<Mutex<dyn MutEventSubscriber + Send>>>>>>,
}

impl Vm {
    /// Create a new VM.
    ///
    /// # Arguments
    ///
    /// * `fd` - The file descriptor for the VMM.
    /// * `config` - The device configuration.
    ///
    /// # Returns
    ///
    /// A `Result` containing the result of the operation.
    pub fn new(fd: i32, config: DeviceConfig) -> Result<Self> {
        // Create the device manager.
        let device_manager = Arc::new(Mutex::new(IoManager::new()));

        // Create the event manager if the data plane is virtio.
        let event_manager = if config.data_plane == "virtio" {
            Some(Arc::new(Mutex::new(
                EventManager::<Arc<Mutex<dyn MutEventSubscriber + Send>>>::new()
                    .map_err(Error::EventManager)?,
            )))
        } else {
            None
        };

        // Create the VM.
        let mut vm = Vm {
            id: config.id as u16,
            device_model: Arc::new(Mutex::new(
                BaoDeviceModel::new(fd, config.id as u16).unwrap(),
            )),
            devices: Vec::new(),
            device_manager,
            event_manager,
        };

        // Add the device.
        // FIXME: For now one VM can have only one device.
        vm.add_device(&config).unwrap();

        Ok(vm)
    }

    /// Add a new device.
    ///
    /// # Arguments
    ///
    /// * `config` - The device configuration.
    ///
    /// # Returns
    ///
    /// A `Result` containing the result of the operation.
    fn add_device(&mut self, config: &DeviceConfig) -> Result<()> {
        // Extract the device type.
        let device_type = VirtioDevType::from(config.device_type.as_str());

        // Extract the data plane.
        let data_plane = VirtioDataPlane::from(config.data_plane.as_str());

        // Clone the device manager, event manager and device model.
        let device_manager = self.device_manager.clone();
        let event_manager = self.event_manager.clone();
        let device_model = self.device_model.clone();

        let device = match device_type {
            // Block device.
            VirtioDevType::Block => match data_plane {
                VirtioDataPlane::Virtio => Ok(VirtioDeviceType::VirtioBlock(
                    VirtioBlock::new(config, device_manager, event_manager, device_model).unwrap(),
                )),
                _ => Err(Error::WrongDeviceConfiguration(
                    VirtioDevType::to_string(&device_type),
                    VirtioDataPlane::to_string(&data_plane),
                )),
            },
            // Virtual Filesystem device.
            VirtioDevType::Fs => match data_plane {
                VirtioDataPlane::VhostUser => Ok(VirtioDeviceType::VhostUserFs(
                    VhostUserFs::new(config, device_manager, event_manager, device_model).unwrap(),
                )),
                _ => Err(Error::WrongDeviceConfiguration(
                    VirtioDevType::to_string(&device_type),
                    VirtioDataPlane::to_string(&data_plane),
                )),
            },
            // Vsock device.
            VirtioDevType::Vsock => match data_plane {
                VirtioDataPlane::Vhost => Ok(VirtioDeviceType::VhostVsock(
                    VhostVsockDevice::new(config, device_manager, event_manager, device_model)
                        .unwrap(),
                )),
                VirtioDataPlane::VhostUser => Ok(VirtioDeviceType::VhostUserVsock(
                    VhostUserVsock::new(config, device_manager, event_manager, device_model)
                        .unwrap(),
                )),
                _ => Err(Error::WrongDeviceConfiguration(
                    VirtioDevType::to_string(&device_type),
                    VirtioDataPlane::to_string(&data_plane),
                )),
            },
            // Network device.
            VirtioDevType::Net => match data_plane {
                VirtioDataPlane::Virtio => Ok(VirtioDeviceType::VirtioNet(
                    VirtioNet::new(config, device_manager, event_manager, device_model).unwrap(),
                )),
                VirtioDataPlane::Vhost => Ok(VirtioDeviceType::VhostNet(
                    VhostNet::new(config, device_manager, event_manager, device_model).unwrap(),
                )),
                _ => Err(Error::WrongDeviceConfiguration(
                    VirtioDevType::to_string(&device_type),
                    VirtioDataPlane::to_string(&data_plane),
                )),
            },
            // Console device.
            VirtioDevType::Console => match data_plane {
                VirtioDataPlane::Virtio => Ok(VirtioDeviceType::VirtioConsole(
                    VirtioConsole::new(config, device_manager, event_manager, device_model)
                        .unwrap(),
                )),
                _ => Err(Error::WrongDeviceConfiguration(
                    VirtioDevType::to_string(&device_type),
                    VirtioDataPlane::to_string(&data_plane),
                )),
            },
            // Input device.
            VirtioDevType::Input => match data_plane {
                VirtioDataPlane::Virtio => Ok(VirtioDeviceType::VirtioInput(
                    VirtioInput::new(config, device_manager, event_manager, device_model).unwrap(),
                )),
                _ => Err(Error::WrongDeviceConfiguration(
                    VirtioDevType::to_string(&device_type),
                    VirtioDataPlane::to_string(&data_plane),
                )),
            },
            // Other device types.
            _ => Err(Error::WrongDeviceConfiguration(
                VirtioDevType::to_string(&device_type),
                VirtioDataPlane::to_string(&data_plane),
            )),
        }
        .unwrap();

        // Push the device to the list of devices.
        self.devices.push(device);

        Ok(())
    }

    /// Run the I/O events.
    ///
    /// # Returns
    ///
    /// A `Result` containing the result of the operation.
    pub fn run_io(self: Arc<Self>) -> Result<()> {
        loop {
            //Attach the I/O client.
            let mut req = match self.device_model.lock().unwrap().attach_io_client() {
                Ok(req) => req,
                Err(err) => {
                    return Err(err);
                }
            };

            //Call the device manager to dispatch the I/O request
            let mut data = (req.value as u32).to_le_bytes();

            match req.op {
                BAO_IO_WRITE => {
                    match self
                        .device_manager
                        .lock()
                        .unwrap()
                        .mmio_write(MmioAddress(req.addr), &mut data)
                    {
                        Ok(()) => {}
                        Err(_err) => {
                            println!("Invalid Mmio write operation: {:?}", req);
                            return Err(Error::InvalidMmioOperation("write"));
                        }
                    }
                }
                BAO_IO_READ => {
                    match self
                        .device_manager
                        .lock()
                        .unwrap()
                        .mmio_read(MmioAddress(req.addr), &mut data)
                    {
                        Ok(()) => {}
                        Err(_err) => {
                            println!("Invalid Mmio read operation: {:?}", req);
                            return Err(Error::InvalidMmioOperation("read"));
                        }
                    }
                }
                _ => {
                    println!("Invalid I/O request direction: {:?}", req.op);
                    return Err(Error::InvalidIoReqDirection(req.op));
                }
            }

            // Update the req.value with the data.
            req.value = u32::from_le_bytes(data) as u64;

            // Notify the I/O client that the I/O request has been completed
            match self.device_model.lock().unwrap().notify_io_completed(req) {
                Ok(()) => {}
                Err(err) => {
                    return Err(err);
                }
            }
        }
    }

    /// Run the event manager.
    ///
    /// # Note
    ///
    /// This method is responsible for running the event manager loop
    /// on a different thread to handle the device events (Ioevenfds)
    /// and to dispatch the respective I/O events to the associated
    /// device.
    pub fn run_event_manager(self: Arc<Self>) {
        loop {
            self.event_manager
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .run()
                .unwrap();
        }
    }
}

// Implementing `Send` trait unsafely for `Vm`.
// This indicates it's considered safe to transfer `Vm` instances between threads,
// enabling transferring ownership of a `Vm` instance between threads.
// As the `Vm` instance is protected by a Mutex, it's safe to transfer ownership of it between threads.
unsafe impl Send for Vm {}

// Implementing `Sync` trait unsafely for `Vm`.
// This indicates it's considered safe to share references of `Vm` between threads.
// As the `Vm` instance is protected by a Mutex, it's safe to share references of it between threads.
unsafe impl Sync for Vm {}
