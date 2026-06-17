use std::cmp;
use std::io::{self, Read, Write};
use std::result;

use log::warn;
use virtio_queue::{DescriptorChain, Queue, QueueOwnedT, QueueT};
use vm_memory::bitmap::AtomicBitmap;
use vm_memory::Bytes;
use vm_memory::GuestAddress;
type GuestMemoryMmap = vm_memory::GuestMemoryMmap<AtomicBitmap>;

use crate::device::SignalUsedQueue;
use crate::net::virtio::tap::Tap;

// According to the standard: "If the VIRTIO_NET_F_GUEST_TSO4, VIRTIO_NET_F_GUEST_TSO6 or
// VIRTIO_NET_F_GUEST_UFO features are used, the maximum incoming packet will be to 65550
// bytes long (the maximum size of a TCP or UDP packet, plus the 14 byte ethernet header),
// otherwise 1514 bytes. The 12-byte struct virtio_net_hdr is prepended to this, making for
// 65562 or 1526 bytes." For transmission, the standard states "The header and packet are added
// as one output descriptor to the transmitq, and the device is notified of the new entry".
// We assume the TX frame will not exceed this size either.
const MAX_BUFFER_SIZE: usize = 65562;

// Prob have to find better names here, but these basically represent the order of the queues.
// If the net device has a single RX/TX pair, then the former has index 0 and the latter 1. When
// the device has multiqueue support, then RX queues have indices 2k, and TX queues 2k+1.
const RXQ_INDEX: u16 = 0;
const TXQ_INDEX: u16 = 1;

#[allow(dead_code)]
#[derive(Debug)]
pub enum Error {
    GuestMemory(vm_memory::GuestMemoryError),
    Queue(virtio_queue::Error),
    Tap(io::Error),
}

impl From<virtio_queue::Error> for Error {
    fn from(e: virtio_queue::Error) -> Self {
        Error::Queue(e)
    }
}

// A simple handler implementation for a RX/TX queue pair, which does not make assumptions about
// the way queue notification is implemented. The backend is not yet generic (we always assume a
// `Tap` object), but we're looking at improving that going forward.
// TODO: Find a better name.

pub struct SimpleHandler<S: SignalUsedQueue> {
    pub driver_notify: S,
    pub rxq: Queue,
    pub rxbuf_current: usize,
    pub rxbuf: [u8; MAX_BUFFER_SIZE],
    pub txq: Queue,
    pub txbuf: [u8; MAX_BUFFER_SIZE],
    pub tap: Tap,
    pub mem: GuestMemoryMmap,
    /// Whether VIRTIO_NET_F_MRG_RXBUF was negotiated (a frame may span multiple RX buffers).
    pub mergeable: bool,
}

impl<S: SignalUsedQueue> SimpleHandler<S> {
    pub fn new(
        driver_notify: S,
        rxq: Queue,
        txq: Queue,
        tap: Tap,
        mem: GuestMemoryMmap,
        mergeable: bool,
    ) -> Self {
        SimpleHandler {
            driver_notify,
            rxq,
            rxbuf_current: 0,
            rxbuf: [0u8; MAX_BUFFER_SIZE],
            txq,
            txbuf: [0u8; MAX_BUFFER_SIZE],
            tap,
            mem,
            mergeable,
        }
    }

    // Have to see how to approach error handling for the `Queue` implementation in particular,
    // because many situations are not really recoverable. We should consider reporting them based
    // on the  metrics/events solution when they appear, and not propagate them further unless
    // it's really useful/necessary.
    fn write_frame_to_guest(&mut self) -> result::Result<bool, Error> {
        let num_bytes = self.rxbuf_current;

        // Per-buffer (head_index, bytes_written) records, returned to the guest after the frame is
        // copied. Fixed-size stack scratch so there is no per-packet heap allocation on the RX hot
        // path. A frame can span at most ceil(MAX_BUFFER_SIZE / min_buffer_size) buffers; 256 is
        // ample for any realistic guest RX buffer size.
        const MAX_RX_BUFFERS: usize = 256;
        let mut used: [(u16, u32); MAX_RX_BUFFERS] = [(0u16, 0u32); MAX_RX_BUFFERS];
        let mut nbufs = 0usize;
        let mut offset = 0usize;
        let mut first_hdr_addr: Option<GuestAddress> = None;

        // Single pass: pull guest RX buffers and copy the frame into them as we go. With
        // VIRTIO_NET_F_MRG_RXBUF a frame may span several buffers; without it we use a single
        // buffer (legacy behaviour) and may truncate an oversized frame.
        {
            let mut iter = self.rxq.iter(&self.mem)?;
            while offset < num_bytes && nbufs < MAX_RX_BUFFERS {
                let chain = match iter.next() {
                    Some(c) => c,
                    None => break,
                };
                let head = chain.head_index();
                let start = offset;
                for desc in chain {
                    if offset >= num_bytes {
                        break;
                    }
                    // RX buffers must be device-writable.
                    if !desc.is_write_only() {
                        continue;
                    }
                    if first_hdr_addr.is_none() {
                        first_hdr_addr = Some(desc.addr());
                    }
                    let len = cmp::min(num_bytes - offset, desc.len() as usize);
                    self.mem
                        .write_slice(&self.rxbuf[offset..offset + len], desc.addr())
                        .map_err(Error::GuestMemory)?;
                    offset += len;
                }
                used[nbufs] = (head, (offset - start) as u32);
                nbufs += 1;

                if !self.mergeable {
                    break;
                }
            }
        }

        // No buffer available at all: leave the frame in rxbuf and retry later.
        if nbufs == 0 {
            return Ok(false);
        }

        // With mergeable buffers the device must report how many buffers the frame spans
        // (num_buffers, at offset 10 of the virtio_net_hdr in the first buffer). The header was
        // already copied above, so patch the field directly in guest memory.
        if self.mergeable {
            if let Some(addr) = first_hdr_addr {
                self.mem
                    .write_slice(&(nbufs as u16).to_le_bytes(), GuestAddress(addr.0 + 10))
                    .map_err(Error::GuestMemory)?;
            }
        }

        // Return each used buffer to the guest with the number of bytes written into it.
        for &(head, len) in used.iter().take(nbufs) {
            self.rxq.add_used(&self.mem, head, len)?;
        }

        if offset != num_bytes {
            // Ran out of guest buffer space; the guest drops the partial frame (TCP retransmits).
            warn!("rx frame too large");
        }

        self.rxbuf_current = 0;

        Ok(true)
    }

    pub fn process_tap(&mut self) -> result::Result<(), Error> {
        loop {
            if self.rxbuf_current == 0 {
                match self.tap.read(&mut self.rxbuf) {
                    Ok(n) => self.rxbuf_current = n,
                    Err(_) => {
                        // TODO: Do something (logs, metrics, etc.) in response to an error when
                        // reading from tap. EAGAIN means there's nothing available to read anymore
                        // (because we open the TAP as non-blocking).
                        break;
                    }
                }
            }

            if !self.write_frame_to_guest()? && !self.rxq.enable_notification(&self.mem)? {
                break;
            }
        }

        if self.rxq.needs_notification(&self.mem)? {
            self.driver_notify.signal_used_queue(RXQ_INDEX);
        }

        Ok(())
    }

    fn send_frame_from_chain(
        &mut self,
        chain: &mut DescriptorChain<&GuestMemoryMmap>,
    ) -> result::Result<u32, Error> {
        let mut count = 0;

        while let Some(desc) = chain.next() {
            let left = self.txbuf.len() - count;
            let len = desc.len() as usize;

            if len > left {
                warn!("tx frame too large");
                break;
            }

            chain
                .memory()
                .read_slice(&mut self.txbuf[count..count + len], desc.addr())
                .map_err(Error::GuestMemory)?;

            count += len;
        }

        self.tap.write(&self.txbuf[..count]).map_err(Error::Tap)?;

        Ok(count as u32)
    }

    pub fn process_txq(&mut self) -> result::Result<(), Error> {
        loop {
            self.txq.disable_notification(&self.mem)?;

            while let Some(mut chain) = self.txq.iter(&self.mem.clone())?.next() {
                self.send_frame_from_chain(&mut chain)?;

                self.txq.add_used(chain.memory(), chain.head_index(), 0)?;

                if self.txq.needs_notification(&self.mem)? {
                    self.driver_notify.signal_used_queue(TXQ_INDEX);
                }
            }

            if !self.txq.enable_notification(&self.mem)? {
                return Ok(());
            }
        }
    }

    pub fn process_rxq(&mut self) -> result::Result<(), Error> {
        self.rxq.disable_notification(&self.mem)?;
        self.process_tap()
    }
}
