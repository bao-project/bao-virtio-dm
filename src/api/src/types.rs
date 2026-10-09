// Copyright (c) Bao Project and Contributors. All rights reserved.
//          João Peixoto <joaopeixotooficial@gmail.com>
//
// SPDX-License-Identifier: Apache-2.0

//! Bao custom types.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// Struct representing a Bao I/O request.
///
/// # Attributes
///
/// * `dm_id` - Device model ID.
/// * `addr` - Address.
/// * `op` - Operation.
/// * `value` - Value.
/// * `access_width` - Access width.
/// * `request_id` - Request ID.
#[repr(C)]
#[derive(Debug)]
pub struct BaoIoRequest {
    pub dm_id: u64,
    pub addr: u64,
    pub op: u64,
    pub value: u64,
    pub access_width: u64,
    pub request_id: u64,
}

/// Struct representing a Bao I/O event file descriptor.
///
/// # Attributes
///
/// * `fd` - File descriptor.
/// * `flags` - Flags.
/// * `addr` - Address.
/// * `len` - Length.
/// * `reserved` - Reserved.
/// * `data` - Datamatch.
#[repr(C)]
pub struct BaoIoEventFd {
    pub fd: u32,
    pub flags: u32,
    pub addr: u64,
    pub len: u32,
    pub reserved: u32,
    pub data: u64,
}

/// Struct representing a Bao IRQ file descriptor.
///
/// # Attributes
///
/// * `fd` - File descriptor.
/// * `flags` - Flags.
#[repr(C)]
pub struct BaoIrqFd {
    pub fd: i32,
    pub flags: u32,
}

/// Struct representing a Bao device model information.
///
/// # Attributes
///
/// * `id` - Device model ID.
/// * `shmem_addr` - Shared memory address.
/// * `shmem_size` - Shared memory size.
/// * `irq` - Device model interrupt.
/// * `fd` - Device model file descriptor.
#[repr(C)]
pub struct BaoDMInfo {
    pub id: u32,
    pub shmem_addr: u64,
    pub shmem_size: u64,
    pub irq: u32,
    pub fd: i32,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
/// Struct representing a Device configuration.
///
/// # Attributes
///
/// * `id` - Device ID.
/// * `type` - Device type.
/// * `shmem_addr` - Shared memory address.
/// * `shmem_size` - Shared memory size.
/// * `shmem_path` - Shared memory path.
/// * `mmio_addr` - MMIO address.
/// * `irq` - Device interrupt.
/// * `data_plane` - Data plane type.
/// * `file_path` - File path (Block device specific option).
/// * `read_only` - Read only (Block device specific option).
/// * `root_device` - Root device (Block device specific option).
/// * `advertise_flush` - Advertise flush (Block device specific option).
/// * `tap_name` - TAP name (Network device specific option).
/// * `mac_addr` - MAC address (Network device specific option).
/// * `guest_cid` - Guest context ID (Vsock device specific option).
/// * `socket_path` - Socket path (Vhost-user device specific option).
pub struct DeviceConfig {
    pub id: u32,
    #[serde(rename = "type")]
    pub device_type: String,
    pub mmio_addr: u64,
    pub data_plane: String,
    // Block device specific fields
    pub file_path: Option<String>,
    pub read_only: Option<bool>,
    pub root_device: Option<bool>,
    pub advertise_flush: Option<bool>,
    // Network device specific fields
    pub tap_name: Option<String>,
    pub mac_addr: Option<String>,
    // Vsock device specific fields
    pub guest_cid: Option<u64>,
    // Vhost-user device specific fields
    pub socket_path: Option<String>,
    // Console device specific fields
    pub pty_alias: Option<String>,
    // Input device specific field: a fixed path for the sink's evdev node
    pub evdev_alias: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq)]
/// Struct representing the VMM configuration.
///
/// # Attributes
///
/// * `devices` - List of devices.
pub struct VMMConfig {
    pub devices: Vec<DeviceConfig>,
}

/// An address either in programmable I/O space or in memory mapped I/O space.
///
/// The `IoEventAddress` is used for specifying the type when registering an event
/// in [register_ioevent](struct.VmFd.html#method.register_ioevent).
pub enum IoEventAddress {
    /// Representation of an programmable I/O address.
    Pio(u64),
    /// Representation of an memory mapped I/O address.
    Mmio(u64),
}
