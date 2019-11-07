use std::{collections::vec_deque::VecDeque, time::Duration};

#[cfg(unix)]
use crate::input::source::tty::TtyInternalEventSource;
#[cfg(windows)]
use crate::input::source::winapi::WinApiEventSource;
use crate::{
    input::{
        events::InternalEvent, poll::EventPoll, poll_timer::PollTimer, source::EventSource, Event,
    },
    Result,
};

/// Can be used to read `InternalEvent`s.
pub(crate) struct InternalEventReader {
    events: VecDeque<InternalEvent>,
    event_source: Box<dyn EventSource>,
}

impl InternalEventReader {
    /// Constructs a new `InternalEventReader`.
    pub(crate) fn new() -> InternalEventReader {
        #[cfg(windows)]
        let event_source = WinApiEventSource::new();
        #[cfg(unix)]
        let event_source =
            TtyInternalEventSource::new().expect("Failed to setup the default event reader.");

        InternalEventReader {
            event_source: Box::new(event_source),
            events: VecDeque::new(),
        }
    }

    /// Swaps the default `EventSource` to the given `EventSource`.
    pub(crate) fn swap_event_source(&mut self, new: Box<dyn EventSource>) {
        self.event_source = new;
    }

    /// Enqueues the given `InternalEvent` onto the internal input buffer.
    pub(crate) fn enqueue(&mut self, event: InternalEvent) {
        self.events.push_back(event);
    }
}

impl EventPoll for InternalEventReader {
    type Output = InternalEvent;

    fn poll(&mut self, timeout: Option<Duration>) -> Result<bool> {
        if !self.events.is_empty() {
            return Ok(true);
        }

        let event = self.event_source.try_read(timeout)?;

        match event {
            None => Ok(false),
            Some(event) => {
                self.events.push_back(event);
                Ok(true)
            }
        }
    }

    fn read(&mut self) -> Result<Self::Output> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(event);
            }

            let _ = self.poll(None)?;
        }
    }
}

/// Can be used to read `Event`s.
pub struct EventReader {
    internal_poll: InternalEventReader,
    events: VecDeque<Event>,
}

impl EventReader {
    /// Constructs an new `EventReader`.
    pub(crate) fn new() -> EventReader {
        EventReader {
            internal_poll: InternalEventReader::new(),
            events: VecDeque::new(),
        }
    }
}

impl EventPoll for EventReader {
    type Output = Event;

    fn poll(&mut self, timeout: Option<Duration>) -> Result<bool> {
        if !self.events.is_empty() {
            return Ok(true);
        }

        let mut timer = PollTimer::new(timeout);

        loop {
            match self.internal_poll.poll(timer.left_over())? {
                true => {
                    match self.internal_poll.read()? {
                        InternalEvent::Event(ev) => {
                            self.events.push_back(ev);
                            return Ok(true);
                        }
                        event => {
                            // write internal event back, we don't need it. But user might.
                            self.internal_poll.enqueue(event)
                        }
                    }
                }
                false => {
                    return Ok(false);
                }
            };

            if timer.elapsed() {
                return Ok(false);
            }
        }
    }

    fn read(&mut self) -> Result<Self::Output> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(event);
            }

            let _ = self.poll(None)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::mpsc::{channel, Sender},
        thread,
        thread::JoinHandle,
        time::Duration,
    };

    use crate::input::{
        events::InternalEvent,
        poll::EventPoll,
        reader::InternalEventReader,
        source::fake::FakeEventSource,
        Event, KeyEvent,
    };

    #[test]
    fn test_internal_poll_with_timeout_should_return() {
        // spin up a thread waiting 2 seconds for input.
        let poll = get_polling_thread(Some(Duration::from_millis(1000)));

        // wait half a second and sent the event
        thread::sleep(Duration::from_millis(500));

        poll.event_sender.send(test_key()).unwrap();

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, true);
        assert_eq!(read, Some(test_key()));
    }

    #[test]
    fn test_internal_poll_with_timeout_should_not_return() {
        // spin up a thread waiting half a seconds for input.
        let poll = get_polling_thread(Some(Duration::from_millis(500)));

        // wait 1 second to exceed the polling thread duration
        thread::sleep(Duration::from_millis(1000));

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, false);
        assert_eq!(read, None);
    }

    #[test]
    fn test_internal_poll_without_timeout_should_return() {
        // spin up a thread waiting 2 seconds for input.
        let poll = get_polling_thread(None);

        // wait 1.5 seconds and then sent the event
        thread::sleep(Duration::from_millis(500));

        poll.event_sender.send(test_key()).unwrap();

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, true);
        assert_eq!(read, Some(test_key()));
    }

    fn test_key() -> InternalEvent {
        InternalEvent::Event(Event::Key(KeyEvent::Char('q')))
    }

    /// Returns the handle to the thread that polls for input as long as the given duration and the sender to trigger the the thread to read the event.
    fn get_polling_thread(timeout: Option<Duration>) -> PollThreadHandleStub {
        let mut reader = InternalEventReader::new();
        let (event_sender, event_receiver) = channel();

        reader.swap_event_source(Box::from(FakeEventSource::new(event_receiver)));

        let handle = thread::spawn(move || {
            let poll_result = reader.poll(timeout).unwrap();

            let read = if poll_result {
                Some(reader.read().unwrap())
            } else {
                None
            };

            (poll_result, read)
        });

        PollThreadHandleStub {
            handle,
            event_sender,
        }
    }

    struct PollThreadHandleStub {
        handle: JoinHandle<(bool, Option<InternalEvent>)>,
        event_sender: Sender<InternalEvent>,
    }
}
