# Bao Hypervisor VirtIO Device Model

## Overview

This repository offers a user space device model, implemented in the Rust programming 
language, enabling integration of various VirtIO device types within the [Bao hypervisor](https://github.com/bao-project/bao-hypervisor) 
across diverse Virtual Machines (VMs), ranging from common VirtIO devices to Vhost and 
Vhost-user backend devices.

## Getting Started

This project can be built either standalone (using a system cross-toolchain) or as part of Buildroot.
The build system does not rely on a hardcoded `.cargo/config.toml`. Instead, the linker is selected via
environment variables, which allows full integration with external build systems such as Buildroot.

To begin utilizing VirtIO device support in Bao Hypervisor, follow these steps

### Prerequisites

- Rust (stable)
- `cargo`
- The appropriate Rust target installed via `rustup`
- A cross-compiler for the desired target architecture

### Install Rust Targets

Install the Rust targets corresponding to the architectures you want to build for:
```bash
rustup target add aarch64-unknown-linux-gnu
rustup target add riscv64gc-unknown-linux-gnu
rustup target add arm-unknown-linux-gnueabi
```

### Install Cross Toolchains

Install the appropriate cross-compilers for your host system.
```bash
sudo apt install \
  gcc-aarch64-linux-gnu \
  gcc-riscv64-linux-gnu \
  gcc-arm-linux-gnueabi
```

### Export the Linker for Cargo

Cargo requires an explicit linker when cross-compiling.
Set the linker using the `CARGO_TARGET_<TRIPLE>_LINKER` environment variable.

1. AArch64 (Linux GNU)
```bash
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
```

RISC-V 64 (Linux GNU)
```bash
export CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER=riscv64-linux-gnu-gcc
```

ARM 32-bit (Linux GNU EABI)
```bash
export CARGO_TARGET_ARM_UNKNOWN_LINUX_GNUEABI_LINKER=arm-linux-gnueabi-gcc
```

### Clone and Build

Clone the repository:
```bash
git clone git@github.com:bao-project/bao-virtio-dm.git
cd bao-virtio-dm
```

1. Build for AArch64 (64-bit ARM)
```bash
cargo build --target=aarch64-unknown-linux-gnu --release
```
Output binary:
```bash
target/aarch64-unknown-linux-gnu/release/bao-virtio-dm

2. Build for RISC-V 64-bit
```bash
cargo build --target=riscv64gc-unknown-linux-gnu --release
```
Output binary:
```bash
target/riscv64gc-unknown-linux-gnu/release/bao-virtio-dm
```

3. Build for ARM 32-bit (EABI)
```bash
cargo build --target=arm-unknown-linux-gnueabi --release
```
Output binary:
```bash
target/arm-unknown-linux-gnueabi/release/bao-virtio-dm
```

## Supported Devices

The full list of supported (and work in progress) devices is presented below:

|                     | DEVICE            | DATAPLANE | SUPPORTED |
| ------------------- | ----------------- | -------   | --- |
| Virtio-Block        | [Block](src/virtio/src/block/README.md)            | VirtIO   | [x](src/virtio/src/block/virtio/README.md) |
| Vhost-User-Fs       | [Virtual File System](src/virtio/src/fs/README.md)            | Vhost-user   | [x](src/virtio/src/fs/vhost_user/README.md) |
| Virtio-Net        | [Net](src/virtio/src/net/README.md)            | VirtIO   | [x](src/virtio/src/net/virtio/README.md) |
| Vhost-Net        | [Net](src/virtio/src/net/README.md)            | Vhost   | - |
| Virtio-Vsock       | [Vsock](src/virtio/src/vsock/README.md)            | VirtIO   | - |
| Vhost-Vsock       | [Vsock](src/virtio/src/vsock/README.md)            | Vhost   | [x](src/virtio/src/vsock/vhost/README.md) |
| Vhost-User-Vsock       | [Vsock](src/virtio/src/vsock/README.md)            | Vhost-user   | [x](src/virtio/src/vsock/vhost_user/README.md) |

## Contributing
Contributions to enhance the functionality and features of Bao Hypervisor VirtIO Device 
Support are welcome. If you have suggestions, bug fixes, or new features to propose, 
feel free to open an issue or submit a pull request.
