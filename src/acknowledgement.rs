use crate::{
    returned_messages::ReturnedMessages,
    wait::{Cancellable, Wait, WaitHandle},
    Error, Result,
};
use parking_lot::Mutex;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use log::debug;

pub type DeliveryTag = u64;

#[derive(Debug, Clone)]
pub(crate) struct Acknowledgements {
    inner: Arc<Mutex<Inner>>,
}

impl Acknowledgements {
    pub(crate) fn new(returned_messages: ReturnedMessages) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::new(returned_messages))),
        }
    }

    pub(crate) fn register_pending(&self, delivery_tag: DeliveryTag) {
        self.inner.lock().register_pending(delivery_tag);
    }

    // Retrieves and removes the Wait<()> that corresponds to the given delivery_tag.
    //
    // When Channel.wait_for_confirm(delivery_tag) is called,
    // the only instance when a wait is removed should be when it is called with this method,
    // and ConfirmationFuture is formed with this Wait.
    pub(crate) fn get_wait(&self, delivery_tag: DeliveryTag) -> Option<Wait<()>> {
        self.inner.lock().waits.remove(&delivery_tag)
    }

    // With the saved last (most recent) delivery_tag,
    // retrieve and remove the corresponding Wait<()>.
    //
    // When Channel.wait_for_confirms() is called,
    // last wait is retrieved but all the other waits should be cleared,
    // since only this last wait will be used to wait for confirms.
    pub(crate) fn get_last_pending(&self) -> Option<Wait<()>> {
        let mut inner = self.inner.lock();
        match inner.last.take() {
            Some(most_recent_delivery_tag) => {
                let last = inner.waits.remove(&most_recent_delivery_tag);
                inner.waits.clear();
                last
            }
            None => None,
        }
    }

    pub(crate) fn ack(&self, delivery_tag: DeliveryTag) -> Result<()> {
        self.inner.lock().ack(delivery_tag)
    }

    pub(crate) fn nack(&self, delivery_tag: DeliveryTag) -> Result<()> {
        self.inner.lock().nack(delivery_tag)
    }

    pub(crate) fn ack_all_pending(&self) {
        let mut inner = self.inner.lock();
        for wait_handle in inner.drain_pending() {
            wait_handle.finish(());
        }
    }

    pub(crate) fn nack_all_pending(&self) {
        let mut inner = self.inner.lock();
        for wait_handle in inner.drain_pending() {
            // call cancel() rather than finish() to ensure that the wait of Confirmation that is Nack'ed receives an error, instead of resolving successfully.
            wait_handle.cancel(Error::UnexpectedReply);
        }
    }

    pub(crate) fn ack_all_before(&self, delivery_tag: DeliveryTag) -> Result<()> {
        let mut inner = self.inner.lock();
        for tag in inner.list_pending_before(delivery_tag) {
            inner.ack(tag)?;
        }
        Ok(())
    }

    pub(crate) fn nack_all_before(&self, delivery_tag: DeliveryTag) -> Result<()> {
        let mut inner = self.inner.lock();
        for tag in inner.list_pending_before(delivery_tag) {
            inner.nack(tag)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Inner {
    last: Option<DeliveryTag>,
    waits: HashMap<DeliveryTag, Wait<()>>,
    pending: HashMap<DeliveryTag, WaitHandle<()>>,
    returned_messages: ReturnedMessages,
}

impl Inner {
    fn new(returned_messages: ReturnedMessages) -> Self {
        Self {
            last: None,
            waits: HashMap::default(),
            pending: HashMap::default(),
            returned_messages,
        }
    }

    // Save both wait and wait_handle in hashmaps with delivery_tags.
    // Wait<()> will be handed to Confirmation when wait_for_confirm() called.
    fn register_pending(&mut self, delivery_tag: DeliveryTag) {
        let (wait, wait_handle) = Wait::new();
        self.waits.insert(delivery_tag, wait);
        self.pending.insert(delivery_tag, wait_handle);
        self.last = Some(delivery_tag);
    }

    fn drop_pending(&mut self, delivery_tag: DeliveryTag, success: bool) -> Result<()> {
        if let Some(delivery_wait_handle) = self.pending.remove(&delivery_tag) {
            if success {
                delivery_wait_handle.finish(());
            } else {
                delivery_wait_handle.cancel(Error::UnexpectedReply);
                // self.returned_messages.register_waiter(delivery_wait_handle);
            }
            Ok(())
        } else {
            Err(Error::PreconditionFailed)
        }
    }

    fn ack(&mut self, delivery_tag: DeliveryTag) -> Result<()> {
        debug!("Message #{} acknowledged.", delivery_tag);
        self.drop_pending(delivery_tag, true)?;
        Ok(())
    }

    fn nack(&mut self, delivery_tag: DeliveryTag) -> Result<()> {
        debug!("Message #{} negatively acknowledged.", delivery_tag);
        self.drop_pending(delivery_tag, false)?;
        Ok(())
    }

    fn drain_pending(&mut self) -> Vec<WaitHandle<()>> {
        self.pending.drain().map(|tup| tup.1).collect()
    }

    fn list_pending_before(&mut self, delivery_tag: DeliveryTag) -> HashSet<DeliveryTag> {
        self.pending
            .iter()
            .map(|tup| tup.0)
            .filter(|tag| **tag <= delivery_tag)
            .cloned()
            .collect()
    }
}
