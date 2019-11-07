//#![deny(unused_imports, unused_must_use)]
//! # Input
//!
//! This documentation does not contain a lot of examples. The reason is that it's fairly
//! obvious how to use this crate. Although, we do provide
//! [examples](https://github.com/crossterm-rs/examples) repository
//! to demonstrate the capabilities.
//!
//! ## Synchronous vs Asynchronous
//!
//! ### Synchronous Reading
//!
//! Read the input synchronously from the user, the reads performed will be blocking calls.
//! Using synchronous over asynchronous reading has the benefit that it is using fewer resources than
//! the asynchronous because background thread and queues are left away.
//!
//! See the [`SyncReader`](struct.SyncReader.html) documentation for more details.
//!
//! ### Asynchronous Reading
//!
//! Read the input asynchronously, input events are gathered in the background and queued for you to read.
//! Using asynchronous reading has the benefit that input events are queued until you read them. You can poll
//! for occurred events, and the reads won't block your program.
//!
//! See the [`AsyncReader`](struct.AsyncReader.html) documentation for more details.
//!
//! ### Technical details
//!
//! On UNIX systems crossterm reads from the TTY, on Windows, it uses `ReadConsoleInputW`.
//! For asynchronous reading, a background thread will be fired up to read input events,
//! occurred events will be queued on an MPSC-channel, and the user can iterate over those events.
//!
//! The terminal has to be in the raw mode, raw mode prevents the input of the user to be displayed
//! on the terminal screen.

use std::{sync::RwLock, time::Duration};

use lazy_static::lazy_static;
use poll::EventPoll;

use crate::{Command, Result};

pub use self::events::{Event, KeyEvent, MouseButton, MouseEvent};

mod ansi;
mod sys;

pub(crate) mod events;
pub(crate) mod poll;
pub(crate) mod poll_timeout;
pub(crate) mod reader;
pub(crate) mod source;

lazy_static! {
    /// Static instance of `EventReader`.
    /// This needs to be static because there can be one internal event reader.
    static ref EVENT_READER: RwLock<reader::EventReader> = { RwLock::new(reader::EventReader::new()) };
}

lazy_static! {
    /// Static instance of `InternalEventReader`.
    /// This needs to be static because there can be one event reader.
    static ref INTERNAL_EVENT_READER: RwLock<reader::InternalEventReader> = { RwLock::new(reader::InternalEventReader::new()) };
}

/// Polls during an given duration for ready events.
///
/// This function takes in an optional duration.
/// * `None`: will block indefinitely until an event is read.
/// * `Some(duration)`: will block for the given duration.
///
/// The following value can be returned:
/// * `Ok(true)`: in case an event is ready.
/// * `Ok(false)`: in case the given duration is elapsed.
/// * `Err(err)`: in case of an error.
///
/// An ready event can be read with [read](LINK)
/// ```no_run
/// use std::time::Duration;
/// use crossterm::{Result, input::poll};
///
/// fn main() -> Result<()> {
///     // poll maximal 1 second
///     if poll(Some(Duration::from_millis(1000)))? {  /* logic */  }
///
///     // poll indefinitely
///     if poll(None)? { /* logic */  }
///
///     Ok(())
/// }
/// ```
pub fn poll(timeout: Option<Duration>) -> Result<bool> {
    let mut reader = EVENT_READER.write().unwrap_or_else(|e| e.into_inner());
    reader.poll(timeout)
}

/// Reads a single event.
///
/// This function will block until an event is received.
/// Use [poll](LINK) to check for ready events.
///
/// ```no_run
/// use crossterm::{Result, input::{read, poll, Event}};
/// use std::time::Duration;
///
/// fn main() -> Result<()> {
///     // poll maximal 1 second for an ready event.
///     if poll(Some(Duration::from_millis(1000)))? {
///         // read the ready event.
///         match read() {
///             Ok(event) => { println!("{:?}", event) }
///             _ => { }
///         }
///      }
///     Ok(())
/// }
/// ```
pub fn read() -> Result<Event> {
    let mut reader = EVENT_READER.write().unwrap_or_else(|e| e.into_inner());
    reader.read()
}

/// Polls to check if there are any `InternalEvent`s that can be read withing the given duration.
pub(crate) fn poll_internal(timeout: Option<Duration>) -> Result<bool> {
    let mut reader = INTERNAL_EVENT_READER
        .write()
        .unwrap_or_else(|e| e.into_inner());
    reader.poll(timeout)
}

/// Reads a single `InternalEvent`.
pub(crate) fn read_internal() -> Result<events::InternalEvent> {
    let mut reader = INTERNAL_EVENT_READER
        .write()
        .unwrap_or_else(|e| e.into_inner());
    reader.read()
}

/// Enqueues an `InternalEvent` into the internal event reader.
#[cfg(unix)]
pub(crate) fn enqueue_internal(event: events::InternalEvent) {
    let mut reader = INTERNAL_EVENT_READER
        .write()
        .unwrap_or_else(|e| e.into_inner());
    reader.enqueue(event)
}

/// Changes the default `EventSource` to the given `EventSource`.
#[cfg(test)]
pub(crate) fn swap_event_source(new: Box<dyn EventSource>) {
    let mut reader = INTERNAL_EVENT_READER
        .write()
        .unwrap_or_else(|e| e.into_inner());
    reader.swap_event_source(new)
}

/// A command that enables mouse mode
///
pub struct EnableMouseCapture;

impl Command for EnableMouseCapture {
    type AnsiType = String;

    fn ansi_code(&self) -> Self::AnsiType {
        ansi::enable_mouse_mode_csi_sequence()
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> Result<()> {
        sys::winapi::enable_mouse_capture()
    }
}

/// A command that disables mouse event monitoring.
///
/// Mouse events will be produced by the
/// [`AsyncReader`](struct.AsyncReader.html)/[`SyncReader`](struct.SyncReader.html).
///
pub struct DisableMouseCapture;

impl Command for DisableMouseCapture {
    type AnsiType = String;

    fn ansi_code(&self) -> Self::AnsiType {
        ansi::disable_mouse_mode_csi_sequence()
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> Result<()> {
        sys::winapi::disable_mouse_capture()
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
        events::{Event, InternalEvent},
        pool::EventPool,
        source::fake::FakeEventSource,
        KeyEvent,
    };

    #[test]
    fn test_internal_poll_with_timeout_should_return() {
        let poll = internal_event_polling_thread(Some(Duration::from_millis(200)));

        sleep_thread_millis(100);

        poll.event_sender.send(test_internal_key()).unwrap();

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, true);
        assert_eq!(read, Some(test_internal_key()));
    }

    #[test]
    fn test_internal_poll_with_timeout_should_not_return() {
        let poll = internal_event_polling_thread(Some(Duration::from_millis(200)));

        sleep_thread_millis(300);

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, false);
        assert_eq!(read, None);
    }

    #[test]
    fn test_internal_poll_without_timeout_should_return() {
        // spin up a thread waiting 2 seconds for input.
        let poll = internal_event_polling_thread(None);

        poll.event_sender.send(test_internal_key()).unwrap();

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, true);
        assert_eq!(read, Some(test_internal_key()));
    }

    #[test]
    fn test_poll_with_timeout_should_return() {
        let poll = event_polling_thread(Some(Duration::from_millis(500)));

        sleep_thread_millis(100);

        poll.event_sender.send(test_internal_key()).unwrap();

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, true);
        assert_eq!(
            Some(InternalEvent::Event(read.unwrap())),
            Some(test_internal_key())
        );
    }

    #[test]
    fn test_poll_with_timeout_should_not_return() {
        let poll = event_polling_thread(Some(Duration::from_millis(200)));

        sleep_thread_millis(300);

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, false);
        assert_eq!(Some(InternalEvent::Event(read.unwrap())), None);
    }

    #[test]
    fn test_poll_without_timeout_should_return() {
        // spin up a thread waiting 2 seconds for input.
        let poll = event_polling_thread(None);

        poll.event_sender.send(test_internal_key()).unwrap();

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, true);
        assert_eq!(
            Some(InternalEvent::Event(read.unwrap())),
            Some(test_internal_key())
        );
    }

    /// Returns the handle to the thread that polls for input as long as the given duration and the sender to trigger the the thread to read the event.
    fn internal_event_polling_thread(
        timeout: Option<Duration>,
    ) -> PollThreadHandleStub<InternalEvent> {
        let (event_sender, event_receiver) = channel();

        let handle = thread::spawn(move || {
            let mut lock = EventPool::get_mut();
            let pool = lock.pool();
            pool.swap_event_source(Box::from(FakeEventSource::new(event_receiver)));

            let poll_result = pool.poll_internal(timeout).unwrap();

            let read = if poll_result {
                Some(pool.read_internal().unwrap())
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

    fn event_polling_thread(timeout: Option<Duration>) -> PollThreadHandleStub<Event> {
        let (event_sender, event_receiver) = channel();

        let handle = thread::spawn(move || {
            let mut lock = EventPool::get_mut();
            let pool = lock.pool();
            println!("set source");
            pool.swap_event_source(Box::from(FakeEventSource::new(event_receiver)));

            let poll_result = pool.poll(timeout).unwrap();

            let read = if poll_result {
                Some(pool.read().unwrap())
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

    struct PollThreadHandleStub<T> {
        handle: JoinHandle<(bool, Option<T>)>,
        event_sender: Sender<InternalEvent>,
    }

    fn sleep_thread_millis(duration: u64) {
        thread::sleep(Duration::from_millis(duration));
    }

    fn test_internal_key() -> InternalEvent {
        InternalEvent::Event(Event::Key(KeyEvent::Char('q')))
    }
}
