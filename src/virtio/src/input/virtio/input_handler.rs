//! Moves what is written into the sink into the frontend's event queue and drains the
//! frontend's status queue.

use super::evdev::{Event, Source};
use crate::device::{SignalUsedQueue, SingleFdSignalQueue};
use event_manager::{EventOps, Events, MutEventSubscriber};
use log::{error, warn};
use std::collections::VecDeque;
use virtio_queue::{Queue, QueueOwnedT, QueueT};
use vm_memory::bitmap::AtomicBitmap;
use vm_memory::{Bytes, GuestAddress};
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::eventfd::EventFd;

type GuestMemoryMmap = vm_memory::GuestMemoryMmap<AtomicBitmap>;

const EVENTQ_INDEX: u16 = 0;
const STATUSQ_INDEX: u16 = 1;
const EVENTQ_DATA: u32 = EVENTQ_INDEX as u32;
const STATUSQ_DATA: u32 = STATUSQ_INDEX as u32;
const SOURCE_DATA: u32 = 0x100;

/// struct virtio_input_event: le16 type, le16 code, le32 value
const VIRTIO_EVENT_SIZE: usize = 8;

pub struct InputHandler {
    driver_notify: SingleFdSignalQueue,
    mem: GuestMemoryMmap,
    eventq: Queue,
    statusq: Queue,
    event_ioeventfd: EventFd,
    status_ioeventfd: EventFd,
    source: Source,
    /// Events read from the sink while the driver had no buffer for them
    pending: VecDeque<[u8; VIRTIO_EVENT_SIZE]>,
}

impl InputHandler {
    pub fn new(
        driver_notify: SingleFdSignalQueue,
        mem: GuestMemoryMmap,
        eventq: Queue,
        statusq: Queue,
        event_ioeventfd: EventFd,
        status_ioeventfd: EventFd,
        source: Source,
    ) -> Self {
        InputHandler {
            driver_notify,
            mem,
            eventq,
            statusq,
            event_ioeventfd,
            status_ioeventfd,
            source,
            pending: VecDeque::new(),
        }
    }

    fn handle_events(&mut self, events: Vec<Event>) {
        for ev in events {
            let mut bytes = [0u8; VIRTIO_EVENT_SIZE];
            bytes[0..2].copy_from_slice(&ev.ev_type.to_le_bytes());
            bytes[2..4].copy_from_slice(&ev.code.to_le_bytes());
            bytes[4..8].copy_from_slice(&(ev.value as u32).to_le_bytes());
            self.pending.push_back(bytes);
        }
        self.flush_pending();
    }

    /// Writes pending events into the buffers the driver queued, one event per buffer
    fn flush_pending(&mut self) {
        let mem = self.mem.clone();
        let mut used = false;
        while !self.pending.is_empty() {
            let chain = match self.eventq.iter(&mem) {
                Ok(mut iter) => iter.next(),
                Err(e) => {
                    error!("virtio-input: event queue: {:?}", e);
                    return;
                }
            };
            let mut chain = match chain {
                Some(chain) => chain,
                None => break,
            };
            let head = chain.head_index();
            let mut written = 0u32;
            if let Some(desc) = chain.next() {
                if desc.is_write_only() && desc.len() as usize >= VIRTIO_EVENT_SIZE {
                    let bytes = self.pending.pop_front().unwrap();
                    if mem.write_slice(&bytes, GuestAddress(desc.addr().0)).is_ok() {
                        written = VIRTIO_EVENT_SIZE as u32;
                    }
                }
            }
            if let Err(e) = self.eventq.add_used(&mem, head, written) {
                error!("virtio-input: add_used: {:?}", e);
                return;
            }
            used = true;
        }
        if used {
            match self.eventq.needs_notification(&mem) {
                Ok(true) | Err(_) => self.driver_notify.signal_used_queue(EVENTQ_INDEX),
                Ok(false) => {}
            }
        }
    }

    /// LED and other status writes from the driver are accepted and dropped
    fn drain_statusq(&mut self) {
        let mem = self.mem.clone();
        let mut used = false;
        loop {
            let chain = match self.statusq.iter(&mem) {
                Ok(mut iter) => iter.next(),
                Err(_) => return,
            };
            let head = match chain {
                Some(chain) => chain.head_index(),
                None => break,
            };
            if self.statusq.add_used(&mem, head, 0).is_err() {
                return;
            }
            used = true;
        }
        if used {
            match self.statusq.needs_notification(&mem) {
                Ok(true) | Err(_) => self.driver_notify.signal_used_queue(STATUSQ_INDEX),
                Ok(false) => {}
            }
        }
    }
}

impl MutEventSubscriber for InputHandler {
    fn process(&mut self, events: Events, _ops: &mut EventOps) {
        let readable = events.event_set().contains(EventSet::IN);
        match events.data() {
            EVENTQ_DATA if readable => {
                let _ = self.event_ioeventfd.read();
                self.flush_pending();
            }
            STATUSQ_DATA if readable => {
                let _ = self.status_ioeventfd.read();
                self.drain_statusq();
            }
            SOURCE_DATA if readable => match self.source.read_events() {
                Ok(evs) => self.handle_events(evs),
                Err(e) => warn!("virtio-input: {}: {}", self.source.path, e),
            },
            _ => {}
        }
    }

    fn init(&mut self, ops: &mut EventOps) {
        ops.add(Events::with_data(
            &self.event_ioeventfd,
            EVENTQ_DATA,
            EventSet::IN,
        ))
        .expect("Failed to register the event queue");
        ops.add(Events::with_data(
            &self.status_ioeventfd,
            STATUSQ_DATA,
            EventSet::IN,
        ))
        .expect("Failed to register the status queue");
        ops.add(Events::with_data(&self.source.file, SOURCE_DATA, EventSet::IN))
            .expect("Failed to register the input sink");
    }
}
