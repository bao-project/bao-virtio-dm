use crate::device::SignalUsedQueue;
use std::fs::File;
use std::result;
use virtio_blk::request::Request;
use virtio_blk::stdio_executor::{self, StdIoBackend};
use virtio_queue::{DescriptorChain, Queue, QueueOwnedT, QueueT};
use vm_memory::bitmap::AtomicBitmap;

type GuestMemoryMmap = vm_memory::GuestMemoryMmap<AtomicBitmap>;

pub struct InOrderQueueHandler<S: SignalUsedQueue> {
    pub driver_notify: S,
    pub mem: GuestMemoryMmap,
    pub queue: Queue,
    pub disk: StdIoBackend<File>,
}

impl<S> InOrderQueueHandler<S>
where
    S: SignalUsedQueue,
{
    /// Process a chain.
    fn process_chain(
        &mut self,
        mut chain: DescriptorChain<&GuestMemoryMmap>,
    ) -> result::Result<(), Error> {
        let used_len = match Request::parse(&mut chain) {
            // Process the backend request.
            Ok(request) => self.disk.process_request(chain.memory(), &request)?,
            Err(e) => {
                println!("block request parse error: {:?}", e);
                0
            }
        };

        // Add the used descriptor to the queue.
        self.queue
            .add_used(chain.memory(), chain.head_index(), used_len)?;

        // Signal the driver, if needed.
        if self.queue.needs_notification(chain.memory())? {
            self.driver_notify.signal_used_queue(0);
        }

        Ok(())
    }

    /// Process the queue.
    ///
    /// # Returns
    ///
    /// * `()` - Ok if the queue was processed successfully.
    pub fn process_queue(&mut self) -> result::Result<(), Error> {
        // To see why this is done in a loop, please look at the `Queue::enable_notification`
        // comments in `virtio_queue`.
        loop {
            // Disable the notifications.
            self.queue.disable_notification(&self.mem)?;

            // Process the queue.
            while let Some(chain) = self.queue.iter(&self.mem.clone())?.next() {
                self.process_chain(chain)?;
            }

            // Enable the notifications.
            if !self.queue.enable_notification(&self.mem)? {
                break;
            }
        }

        Ok(())
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum Error {
    GuestMemory(vm_memory::GuestMemoryError),
    Queue(virtio_queue::Error),
    ProcessRequest(stdio_executor::ProcessReqError),
}

impl From<vm_memory::GuestMemoryError> for Error {
    fn from(e: vm_memory::GuestMemoryError) -> Self {
        Error::GuestMemory(e)
    }
}

impl From<virtio_queue::Error> for Error {
    fn from(e: virtio_queue::Error) -> Self {
        Error::Queue(e)
    }
}

impl From<stdio_executor::ProcessReqError> for Error {
    fn from(e: stdio_executor::ProcessReqError) -> Self {
        Error::ProcessRequest(e)
    }
}
