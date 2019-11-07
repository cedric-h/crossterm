#![deny(unused_imports, unused_must_use)]

//! # Input
//!
//! The `input` module provides a functionality to read the input events.
//!
//! This documentation does not contain a lot of examples. The reason is that it's fairly
//! obvious how to use this crate. Although, we do provide
//! [examples](https://github.com/crossterm-rs/examples) repository
//! to demonstrate the capabilities.
//!
//! TODO: poll/read Api and implementation details.

use std::{sync::RwLock, time::Duration};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use lazy_static::lazy_static;
use poll::EventPoll;

use crate::{Command, Result};

mod ansi;
mod poll;
mod timeout;
mod reader;
mod source;
mod sys;

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
pub(crate) fn read_internal() -> Result<InternalEvent> {
    let mut reader = INTERNAL_EVENT_READER
        .write()
        .unwrap_or_else(|e| e.into_inner());
    reader.read()
}

/// Enqueues an `InternalEvent` into the internal event reader.
#[cfg(unix)]
pub(crate) fn enqueue_internal(event: InternalEvent) {
    let mut reader = INTERNAL_EVENT_READER
        .write()
        .unwrap_or_else(|e| e.into_inner());
    reader.enqueue(event)
}

/// Changes the default `EventSource` to the given `EventSource`.
#[cfg(test)]
pub(crate) fn swap_event_source(new: Box<dyn source::EventSource>) {
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

/// Represents an input event.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, PartialOrd, PartialEq, Hash, Clone)]
pub enum Event {
    /// A single key or a combination of keys.
    Key(KeyEvent),
    /// A mouse event.
    Mouse(MouseEvent),
    /// An unknown event.
    Unknown,
}

/// Represents a mouse event.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, PartialOrd, PartialEq, Hash, Clone, Copy)]
pub enum MouseEvent {
    /// Pressed mouse button at the location (column, row).
    Press(MouseButton, u16, u16),
    /// Released mouse button at the location (column, row).
    Release(u16, u16),
    /// Mouse moved with a pressed left button to the new location (column, row).
    Hold(u16, u16),
    /// An unknown mouse event.
    Unknown,
}

/// Represents a mouse button/wheel.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, PartialOrd, PartialEq, Hash, Clone, Copy)]
pub enum MouseButton {
    /// Left mouse button.
    Left,
    /// Right mouse button.
    Right,
    /// Middle mouse button.
    Middle,
    /// Wheel scrolled up.
    WheelUp,
    /// Wheel scrolled down.
    WheelDown,
}

/// Represents a key or a combination of keys.
#[derive(Debug, PartialOrd, PartialEq, Eq, Clone, Copy, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum KeyEvent {
    /// Backspace key.
    Backspace,
    /// Enter key.
    Enter,
    /// Left arrow key.
    Left,
    /// Right arrow key.
    Right,
    /// Up arrow key.
    Up,
    /// Down arrow key.
    Down,
    /// Home key.
    Home,
    /// End key.
    End,
    /// Page up key.
    PageUp,
    /// Page dow key.
    PageDown,
    /// Tab key.
    Tab,
    /// Shift + Tab key.
    BackTab,
    /// Delete key.
    Delete,
    /// Insert key.
    Insert,
    /// F key.
    ///
    /// `KeyEvent::F(1)` represents F1 key, etc.
    F(u8),
    /// A character.
    ///
    /// `KeyEvent::Char('c')` represents `c` character, etc.
    Char(char),
    /// Alt key + character.
    ///
    /// `KeyEvent::Alt('c')` represents `Alt + c`, etc.
    Alt(char),
    /// Ctrl key + character.
    ///
    /// `KeyEvent::Ctrl('c') ` represents `Ctrl + c`, etc.
    Ctrl(char),
    /// Null.
    Null,
    /// Escape key.
    Esc,
    /// Ctrl + up arrow key.
    CtrlUp,
    /// Ctrl + down arrow key.
    CtrlDown,
    /// Ctrl + right arrow key.
    CtrlRight,
    /// Ctrl + left arrow key.
    CtrlLeft,
    /// Shift + up arrow key.
    ShiftUp,
    /// Shift + down arrow key.
    ShiftDown,
    /// Shift + right arrow key.
    ShiftRight,
    /// Shift + left arrow key.
    ShiftLeft,
}

/// An internal event.
///
/// Encapsulates publicly available `InputEvent` with additional internal
/// events that shouldn't be publicly available to the crate users.
#[derive(Debug, PartialOrd, PartialEq, Hash, Clone)]
pub(crate) enum InternalEvent {
    /// An input event.
    Event(Event),
    /// A cursor position (`col`, `row`).
    #[cfg(unix)]
    CursorPosition(u16, u16),
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
        poll, poll_internal, read, read_internal, source::fake::FakeEventSource, swap_event_source,
        Event, InternalEvent, KeyEvent,
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
        assert_eq!(read, None);
    }

    #[test]
    fn test_poll_without_timeout_should_return() {
        let poll = event_polling_thread(None);

        poll.event_sender.send(test_internal_key()).unwrap();

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, true);
        assert_eq!(
            Some(InternalEvent::Event(read.unwrap())),
            Some(test_internal_key())
        );
    }

    #[test]
    #[cfg(unix)]
    fn test_event_should_not_thrown_away() {
        // first sent cursor position event, and try to poll with `EventReader`
        let poll = event_polling_thread(Some(Duration::from_millis(500)));

        poll.event_sender
            .send(InternalEvent::CursorPosition(5, 5))
            .unwrap();

        let (poll_result, read) = poll.handle.join().unwrap();

        assert_eq!(poll_result, false);
        assert_eq!(read, None);

        // Then try to read with `InternalEventReader`, the cursor position event should still be in cache.
        let internal_poll = internal_event_polling_thread(Some(Duration::from_millis(500)));

        let (poll_result, read) = internal_poll.handle.join().unwrap();

        assert_eq!(poll_result, true);
        assert_eq!(read, Some(InternalEvent::CursorPosition(5, 5)));
    }

    /// Returns the handle to the thread that polls for input as long as the given duration and the sender to trigger the the thread to read the event.
    fn internal_event_polling_thread(
        timeout: Option<Duration>,
    ) -> PollThreadHandleStub<InternalEvent> {
        let (event_sender, event_receiver) = channel();

        let handle = thread::spawn(move || {
            swap_event_source(Box::from(FakeEventSource::new(event_receiver)));

            let poll_result = poll_internal(timeout).unwrap();

            let read = if poll_result {
                Some(read_internal().unwrap())
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
            swap_event_source(Box::from(FakeEventSource::new(event_receiver)));

            let poll_result = poll(timeout).unwrap();

            let read = if poll_result {
                Some(read().unwrap())
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
