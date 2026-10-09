// Copyright (c) Bao Project and Contributors. All rights reserved.
//          João Peixoto <joaopeixotooficial@gmail.com>
//
// SPDX-License-Identifier: Apache-2.0

//! Bao device model.

use crate::defines::{BAO_IO_ASK, BAO_IRQFD_FLAG_ASSIGN};
use crate::error::{Error, Result};
use crate::ioctl::*;
use crate::types::{BaoDMInfo, BaoIoEventFd, BaoIoRequest, BaoIrqFd};
use libc::ioctl;
use std::os::fd::AsRawFd;
use vmm_sys_util::errno;
use vmm_sys_util::eventfd::EventFd;

/// Bao Hypervisor Device Model.
///
/// # Attributes
///
/// * `fd` - The file descriptor for the VMM.
/// * `devmodel_fd` - The file descriptor for the device model.
/// * `id` - The ID of the device model.
#[derive(Clone)]
pub struct BaoDeviceModel {
    pub fd: i32,
    pub devmodel_fd: i32,
    pub id: u16,
    pub shmem_addr: u64,
    pub shmem_size: u64,
    pub irq: u32,
}

impl BaoDeviceModel {
    /// Create a new device model.
    ///
    /// # Arguments
    ///
    /// * `fd` - The file descriptor for the VMM.
    /// * `id` - The ID of the device model.
    ///
    /// # Returns
    ///
    /// A `Result` containing the result of the operation.
    pub fn new(fd: i32, id: u16) -> Result<Self> {
        let mut dm_info = BaoDMInfo {
            id: id as u32,
            shmem_addr: 0,
            shmem_size: 0,
            irq: 0,
            fd: 0,
        };

        unsafe {
            let ret = ioctl(fd, BAO_IOCTL_IO_DM_GET_INFO() as _, &mut dm_info);

            if ret < 0 {
                return Err(Error::OpenFdFailed(
                    "dm_info",
                    std::io::Error::last_os_error(),
                ));
            }
        }

        // Create the device model object.
        let device_model = BaoDeviceModel {
            fd: fd,
            devmodel_fd: dm_info.fd,
            id: id,
            shmem_addr: dm_info.shmem_addr,
            shmem_size: dm_info.shmem_size,
            irq: dm_info.irq,
        };

        Ok(device_model)
    }

    /// Attach the I/O client to the VM.
    ///
    /// # Returns
    ///
    /// A `Result` containing the result of the operation.
    pub fn attach_io_client(&self) -> Result<BaoIoRequest> {
        // Create a new I/O request
        let mut request = BaoIoRequest {
            dm_id: 0,
            addr: 0,
            op: BAO_IO_ASK,
            value: 0,
            access_width: 0,
            request_id: 0,
        };
        unsafe {
            let ret = ioctl(self.devmodel_fd, BAO_IOCTL_IO_ATTACH_CLIENT() as _, &mut request);

            if ret < 0 {
                return Err(Error::BaoIoctlError(
                    std::io::Error::last_os_error(),
                    std::any::type_name::<Self>(),
                ));
            }
        }
        Ok(request)
    }

    /// Notifies I/O request completion.
    ///
    /// # Arguments
    ///
    /// * `req` - The BaoIoRequest to be notified.
    ///
    /// # Return
    ///
    /// * `Result<()>` - A Result containing Ok(()) on success, or an Error on failure.
    pub fn notify_io_completed(&self, req: BaoIoRequest) -> Result<()> {
        // Notify I/O request completion
        unsafe {
            let ret = ioctl(
                self.devmodel_fd,
                BAO_IOCTL_IO_REQUEST_NOTIFY_COMPLETED() as _,
                &req,
            );

            if ret < 0 {
                return Err(Error::BaoIoctlError(
                    std::io::Error::last_os_error(),
                    std::any::type_name::<Self>(),
                ));
            }
        }

        // Return Ok(()) on success
        Ok(())
    }

    /// Registers an ioeventfd within the VM (guest to host interrupt)
    ///
    /// # Arguments
    ///
    /// * `kick` - The EventFd to be registered.
    /// * `flags` - The flags to be used.
    /// * `addr` - The address to be registered.
    /// * `datamatch` - The data to be matched (index of the Virtqueue).
    ///
    /// # Returns
    ///
    /// A `Result` containing the result of the operation.
    pub fn register_ioeventfd(
        &self,
        kick: u32,
        flags: u32,
        addr: u64,
        datamatch: u64,
    ) -> Result<()> {
        // Create a BaoIoEventFd struct.
        let ioeventfd = BaoIoEventFd {
            fd: kick,
            flags: flags,
            addr: addr,
            len: 4,
            reserved: 0,
            data: datamatch, // Index of the Virtqueue to match with the 'value' field of the 'bao_io_request' struct
        };

        // Call the ioctl to register the ioeventfd.
        unsafe {
            let ret = ioctl(self.devmodel_fd, BAO_IOCTL_IOEVENTFD() as _, &ioeventfd);

            if ret < 0 {
                return Err(Error::RegisterIoevent(errno::Error::last()));
            }
        }
        Ok(())
    }

    /// Registers an irqfd within the VM (host to guest interrupt)
    ///
    /// # Arguments
    ///
    /// * `call` - The EventFd to be registered.
    ///
    /// # Returns
    ///
    /// A `Result` containing the result of the operation.
    pub fn register_irqfd(&self, call: &EventFd) -> Result<()> {
        // Create a BaoIrqFd struct.
        let irqfd = BaoIrqFd {
            fd: call.as_raw_fd() as i32,
            flags: BAO_IRQFD_FLAG_ASSIGN, // Assign the Irqfd
        };

        // Call the ioctl to register the irqfd.
        unsafe {
            let ret = ioctl(self.devmodel_fd, BAO_IOCTL_IRQFD() as _, &irqfd);

            if ret < 0 {
                return Err(Error::RegisterIrqfd(errno::Error::last()));
            }
        }
        Ok(())
    }
}
