use std::{
    sync::{mpsc::Receiver, Mutex},
    time::Duration,
};

use crate::{
    input::{source::EventSource, InternalEvent},
    Result,
};

/// This event source can be used for test purposes. And gives you direct control over the events read by crossterm.
pub(crate) struct FakeEventSource {
    input_receiver: Mutex<Receiver<InternalEvent>>,
}

impl FakeEventSource {
    /// Constructs a new `FakeEventSource` with the given `Receiver`, use the sender to trigger the event reader..
    pub(crate) fn new(input_receiver: Receiver<InternalEvent>) -> FakeEventSource {
        FakeEventSource {
            input_receiver: Mutex::new(input_receiver),
        }
    }
}

impl EventSource for FakeEventSource {
    fn try_read(&mut self, timeout: Option<Duration>) -> Result<Option<InternalEvent>> {
        println!("read: {:?}", timeout);
        if let Some(timeout) = timeout {
            if let Ok(val) = self.input_receiver.lock().unwrap().recv_timeout(timeout) {
                println!("c");
                return Ok(Some(val));
            } else {
                println!("b");
                return Ok(None);
            }
        } else {
            return Ok(Some(self.input_receiver.lock().unwrap().recv().unwrap()));
        }
    }
}
