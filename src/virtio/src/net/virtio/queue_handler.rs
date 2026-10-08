use event_manager::{EventOps, Events, MutEventSubscriber};
use log::error;
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::eventfd::EventFd;

use crate::device::SingleFdSignalQueue;

use super::simple_handler::SimpleHandler;

const TAPFD_DATA: u32 = 0;
const RX_IOEVENT_DATA: u32 = 1;
const TX_IOEVENT_DATA: u32 = 2;

/// This object simply combines the more generic `SimpleHandler` with a two concrete queue
/// signalling implementation based on `EventFd`s, and then also implements `MutEventSubscriber`
/// to interact with the event manager. `ioeventfd` is the `EventFd` connected to queue
/// notifications coming from the driver.
/// TODO: Extend this to support multiqueue.
pub struct QueueHandler {
    pub inner: SimpleHandler<SingleFdSignalQueue>,
    pub rx_ioevent: EventFd,
    pub tx_ioevent: EventFd,
}

impl QueueHandler {
    // Helper method that receives an error message to be logged and the `ops` handle
    // which is used to unregister all events.
    fn handle_error<S: AsRef<str>>(&self, s: S, ops: &mut EventOps) {
        error!("{}", s.as_ref());
        ops.remove(Events::empty(&self.rx_ioevent))
            .expect("Failed to remove rx ioevent");
        ops.remove(Events::empty(&self.tx_ioevent))
            .expect("Failed to remove tx ioevent");
        ops.remove(Events::empty(&self.inner.tap))
            .expect("Failed to remove tap event");
    }
}

impl MutEventSubscriber for QueueHandler {
    fn process(&mut self, events: Events, ops: &mut EventOps) {
        // TODO: We can also consider panicking on the errors that cannot be generated
        // or influenced.

        if events.event_set() != EventSet::IN {
            self.handle_error("Unexpected event_set", ops);
            return;
        }

        match events.data() {
            // Data-path errors are logged and the device keeps running: the driver sees a
            // dropped frame, not a dead adapter
            TAPFD_DATA => {
                if let Err(e) = self.inner.process_tap() {
                    error!("Process tap error {:?}", e);
                }
            }
            RX_IOEVENT_DATA => {
                if self.rx_ioevent.read().is_err() {
                    self.handle_error("Rx ioevent read", ops);
                } else if let Err(e) = self.inner.process_rxq() {
                    error!("Process rx error {:?}", e);
                }
            }
            TX_IOEVENT_DATA => {
                if self.tx_ioevent.read().is_err() {
                    self.handle_error("Tx ioevent read", ops);
                }
                if let Err(e) = self.inner.process_txq() {
                    error!("Process tx error {:?}", e);
                }
            }
            _ => self.handle_error("Unexpected data", ops),
        }
    }

    fn init(&mut self, ops: &mut EventOps) {
        ops.add(Events::with_data(
            &self.inner.tap,
            TAPFD_DATA,
            EventSet::IN | EventSet::EDGE_TRIGGERED,
        ))
        .expect("Unable to add tapfd");

        ops.add(Events::with_data(
            &self.rx_ioevent,
            RX_IOEVENT_DATA,
            EventSet::IN,
        ))
        .expect("Unable to add rxfd");

        ops.add(Events::with_data(
            &self.tx_ioevent,
            TX_IOEVENT_DATA,
            EventSet::IN,
        ))
        .expect("Unable to add txfd");
    }
}
