use std::os::unix::io::IntoRawFd;
use std::os::unix::io::RawFd;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc, Mutex,
};
use std::time::Duration;
use std::{fs, io, thread};

use libc::{c_int, c_void, size_t, ssize_t};
use mio::unix::EventedFd;
use mio::{Events, Poll, PollOpt, Ready, Token};

use lazy_static::lazy_static;

use crate::input::events::InternalEvent;
use crate::{ErrorKind, Result};
use crate::{Event, KeyEvent, MouseButton, MouseEvent};

use self::utils::{check_for_error, check_for_error_result};

lazy_static! {
    /// A shared internal event provider.
    static ref INTERNAL_EVENT_PROVIDER: Mutex<Box<dyn InternalEventProvider>> =
        Mutex::new(default_internal_event_provider());
}

// TODO 1.0: Enhance utils::sys::unix::wrap_with_result and use it
mod utils {
    use std::io;

    use libc::c_int;

    pub fn check_for_error(result: c_int) -> io::Result<()> {
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    // TODO 1.0: Enhance utils::sys::unix::wrap_with_result and use it
    pub fn check_for_error_result(result: c_int) -> io::Result<libc::c_int> {
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result)
        }
    }
}

/// A file descriptor wrapper.
///
/// It allows to retrieve raw file descriptor, write to the file descriptor and
/// mainly it closes the file descriptor once dropped.
pub struct FileDesc {
    fd: RawFd,
    close_on_drop: bool,
}

impl FileDesc {
    fn new(fd: RawFd) -> FileDesc {
        FileDesc::with_close_on_drop(fd, true)
    }

    fn with_close_on_drop(fd: RawFd, close_on_drop: bool) -> FileDesc {
        FileDesc { fd, close_on_drop }
    }

    fn read_byte(&self) -> Result<u8> {
        let mut buf: [u8; 1] = [0];
        utils::check_for_error(unsafe {
            libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, 1) as c_int
        })?;

        Ok(buf[0])
    }

    fn write(&self, buf: &[u8]) -> io::Result<usize> {
        // libstd::sys::unix::fd.rs

        let ret = check_for_error_result(unsafe {
            libc::write(
                self.fd,
                buf.as_ptr() as *const c_void,
                std::cmp::min(buf.len(), max_len()) as size_t,
            ) as c_int
        })?;
        Ok(ret as usize)
    }

    fn raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for FileDesc {
    // libstd::sys::unix::fd.rs

    fn drop(&mut self) {
        if self.close_on_drop {
            // Note that errors are ignored when closing a file descriptor. The
            // reason for this is that if an error occurs we don't actually know if
            // the file descriptor was closed or not, and if we retried (for
            // something like EINTR), we might close another valid file descriptor
            // opened after we closed ours.
            let _ = unsafe { libc::close(self.fd) };
        }
    }
}

/// Creates a file descriptor pointing to the standard input or `/dev/tty`.
pub fn tty_fd() -> Result<FileDesc> {
    let (fd, close_on_drop) = if unsafe { libc::isatty(libc::STDIN_FILENO) == 1 } {
        (libc::STDIN_FILENO, false)
    } else {
        (
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/tty")?
                .into_raw_fd(),
            true,
        )
    };

    Ok(FileDesc::with_close_on_drop(fd, close_on_drop))
}

pub struct TtyPoll {
    poll: Poll,
    tty_fd: FileDesc,
    events: Events,
}

// Tokens to identify file descriptor
const TTY_TOKEN: Token = Token(0);

impl TtyPoll {
    pub fn new(tty_fd: FileDesc) -> TtyPoll {
        // Get raw file descriptors for
        let tty_raw_fd = tty_fd.raw_fd();

        // Setup polling with raw file descriptors
        let tty_ev = EventedFd(&tty_raw_fd);

        let poll = Poll::new().unwrap();
        poll.register(&tty_ev, TTY_TOKEN, Ready::readable(), PollOpt::level())
            .unwrap();

        TtyPoll {
            poll,
            tty_fd,
            events: Events::with_capacity(2),
        }
    }

    pub(crate) fn poll(&mut self, timeout: Option<Duration>) -> Result<bool> {
        Ok(self.poll.poll(&mut self.events, timeout).map(|x| x > 0)?)
    }

    pub(crate) fn read(&mut self) -> Result<Option<InternalEvent>> {
        let mut buffer: Vec<u8> = Vec::with_capacity(32);

        let get_tokens =
            |events: &Events| -> Vec<Token> { events.iter().map(|ev| ev.token()).collect() };

        loop {
            // Get tokens to identify file descriptors
            let tokens = get_tokens(&self.events);

            if tokens.contains(&TTY_TOKEN) {
                // There's an event on tty
                if let Ok(byte) = self.tty_fd.read_byte() {
                    // Poll again to check if there's still anything to read when we read one byte.
                    // This time with 0 timeout which means return immediately.
                    //
                    // We need this information to distinguish between Esc key and possible
                    // Esc sequence.
                    self.poll(Some(Duration::from_millis(0)))?;

                    let tokens = get_tokens(&self.events);

                    let input_available = tokens.contains(&TTY_TOKEN);

                    buffer.push(byte);
                    match parse_event(&buffer, input_available) {
                        // Not enough info to parse the event, wait for more bytes
                        Ok(None) => {}
                        // Clear the input buffer and send the event
                        Ok(Some(event)) => {
                            buffer.clear();
                            return Ok(Some(event));
                        }
                        // Malformed sequence, clear the buffer
                        Err(_) => buffer.clear(),
                    }
                }
            }
        }
        Ok(None)
    }
}

//
// Event parsing
//
// This code (& previous one) are kind of ugly. We have to think about this,
// because it's really not maintainable, no tests, etc.
//
// Every fn returns Result<Option<InputEvent>>
//
// Ok(None) -> wait for more bytes
// Err(_) -> failed to parse event, clear the buffer
// Ok(Some(event)) -> we have event, clear the buffer
//

fn could_not_parse_event_error() -> ErrorKind {
    ErrorKind::IoError(io::Error::new(
        io::ErrorKind::Other,
        "Could not parse an event",
    ))
}

fn parse_event(buffer: &[u8], input_available: bool) -> Result<Option<InternalEvent>> {
    if buffer.is_empty() {
        return Ok(None);
    }

    match buffer[0] {
        b'\x1B' => {
            if buffer.len() == 1 {
                if input_available {
                    // Possible Esc sequence
                    Ok(None)
                } else {
                    Ok(Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Esc))))
                }
            } else {
                match buffer[1] {
                    b'O' => {
                        if buffer.len() == 2 {
                            Ok(None)
                        } else {
                            match buffer[2] {
                                // F1-F4
                                val @ b'P'..=b'S' => Ok(Some(InternalEvent::Input(
                                    Event::Keyboard(KeyEvent::F(1 + val - b'P')),
                                ))),
                                _ => Err(could_not_parse_event_error()),
                            }
                        }
                    }
                    b'[' => parse_csi(buffer),
                    b'\x1B' => Ok(Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Esc)))),
                    _ => parse_utf8_char(&buffer[1..]).map(|maybe_char| {
                        maybe_char
                            .map(KeyEvent::Alt)
                            .map(Event::Keyboard)
                            .map(InternalEvent::Input)
                    }),
                }
            }
        }
        b'\r' | b'\n' => Ok(Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Enter)))),
        b'\t' => Ok(Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Tab)))),
        b'\x7F' => Ok(Some(InternalEvent::Input(Event::Keyboard(
            KeyEvent::Backspace,
        )))),
        c @ b'\x01'..=b'\x1A' => Ok(Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Ctrl(
            (c as u8 - 0x1 + b'a') as char,
        ))))),
        c @ b'\x1C'..=b'\x1F' => Ok(Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Ctrl(
            (c as u8 - 0x1C + b'4') as char,
        ))))),
        b'\0' => Ok(Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Null)))),
        _ => parse_utf8_char(buffer).map(|maybe_char| {
            maybe_char
                .map(KeyEvent::Char)
                .map(Event::Keyboard)
                .map(InternalEvent::Input)
        }),
    }
}

fn parse_csi(buffer: &[u8]) -> Result<Option<InternalEvent>> {
    assert!(buffer.starts_with(&[b'\x1B', b'['])); // ESC [

    if buffer.len() == 2 {
        return Ok(None);
    }

    let input_event = match buffer[2] {
        b'[' => {
            if buffer.len() == 3 {
                None
            } else {
                match buffer[3] {
                    // NOTE (@imdaveho): cannot find when this occurs;
                    // having another '[' after ESC[ not a likely scenario
                    val @ b'A'..=b'E' => Some(Event::Keyboard(KeyEvent::F(1 + val - b'A'))),
                    _ => Some(Event::Unknown),
                }
            }
        }
        b'D' => Some(Event::Keyboard(KeyEvent::Left)),
        b'C' => Some(Event::Keyboard(KeyEvent::Right)),
        b'A' => Some(Event::Keyboard(KeyEvent::Up)),
        b'B' => Some(Event::Keyboard(KeyEvent::Down)),
        b'H' => Some(Event::Keyboard(KeyEvent::Home)),
        b'F' => Some(Event::Keyboard(KeyEvent::End)),
        b'Z' => Some(Event::Keyboard(KeyEvent::BackTab)),
        b'M' => return parse_csi_x10_mouse(buffer),
        b'<' => return parse_csi_xterm_mouse(buffer),
        b'0'..=b'9' => {
            // Numbered escape code.
            if buffer.len() == 3 {
                None
            } else {
                // The final byte of a CSI sequence can be in the range 64-126, so
                // let's keep reading anything else.
                let last_byte = *buffer.last().unwrap();
                if last_byte < 64 || last_byte > 126 {
                    None
                } else {
                    match buffer[buffer.len() - 1] {
                        b'M' => return parse_csi_rxvt_mouse(buffer),
                        b'~' => return parse_csi_special_key_code(buffer),
                        b'R' => return parse_csi_cursor_position(buffer),
                        _ => return parse_csi_modifier_key_code(buffer),
                    }
                }
            }
        }
        _ => Some(Event::Unknown),
    };

    Ok(input_event.map(InternalEvent::Input))
}

fn next_parsed<T>(iter: &mut dyn Iterator<Item = &str>) -> Result<T>
where
    T: std::str::FromStr,
{
    iter.next()
        .ok_or_else(|| could_not_parse_event_error())?
        .parse::<T>()
        .map_err(|_| could_not_parse_event_error())
}

fn parse_csi_cursor_position(buffer: &[u8]) -> Result<Option<InternalEvent>> {
    // ESC [ Cy ; Cx R
    //   Cy - cursor row number (starting from 1)
    //   Cx - cursor column number (starting from 1)
    assert!(buffer.starts_with(&[b'\x1B', b'['])); // ESC [
    assert!(buffer.ends_with(&[b'R']));

    let s = std::str::from_utf8(&buffer[2..buffer.len() - 1])
        .map_err(|_| could_not_parse_event_error())?;

    let mut split = s.split(';');

    let y = next_parsed::<u16>(&mut split)? - 1;
    let x = next_parsed::<u16>(&mut split)? - 1;

    Ok(Some(InternalEvent::CursorPosition(x, y)))
}

fn parse_csi_modifier_key_code(buffer: &[u8]) -> Result<Option<InternalEvent>> {
    assert!(buffer.starts_with(&[b'\x1B', b'['])); // ESC [

    let modifier = buffer[buffer.len() - 2];
    let key = buffer[buffer.len() - 1];

    let input_event = match (modifier, key) {
        (53, 65) => Event::Keyboard(KeyEvent::CtrlUp),
        (53, 66) => Event::Keyboard(KeyEvent::CtrlDown),
        (53, 67) => Event::Keyboard(KeyEvent::CtrlRight),
        (53, 68) => Event::Keyboard(KeyEvent::CtrlLeft),
        (50, 65) => Event::Keyboard(KeyEvent::ShiftUp),
        (50, 66) => Event::Keyboard(KeyEvent::ShiftDown),
        (50, 67) => Event::Keyboard(KeyEvent::ShiftRight),
        (50, 68) => Event::Keyboard(KeyEvent::ShiftLeft),
        _ => Event::Unknown,
    };

    Ok(Some(InternalEvent::Input(input_event)))
}

fn parse_csi_special_key_code(buffer: &[u8]) -> Result<Option<InternalEvent>> {
    assert!(buffer.starts_with(&[b'\x1B', b'['])); // ESC [
    assert!(buffer.ends_with(&[b'~']));

    let s = std::str::from_utf8(&buffer[2..buffer.len() - 1])
        .map_err(|_| could_not_parse_event_error())?;
    let mut split = s.split(';');

    // This CSI sequence can be a list of semicolon-separated numbers.
    let first = next_parsed::<u8>(&mut split)?;

    if next_parsed::<u8>(&mut split).is_ok() {
        // TODO: handle multiple values for key modifiers (ex: values [3, 2] means Shift+Delete)
        return Ok(Some(InternalEvent::Input(Event::Unknown)));
    }

    let input_event = match first {
        1 | 7 => Event::Keyboard(KeyEvent::Home),
        2 => Event::Keyboard(KeyEvent::Insert),
        3 => Event::Keyboard(KeyEvent::Delete),
        4 | 8 => Event::Keyboard(KeyEvent::End),
        5 => Event::Keyboard(KeyEvent::PageUp),
        6 => Event::Keyboard(KeyEvent::PageDown),
        v @ 11..=15 => Event::Keyboard(KeyEvent::F(v - 10)),
        v @ 17..=21 => Event::Keyboard(KeyEvent::F(v - 11)),
        v @ 23..=24 => Event::Keyboard(KeyEvent::F(v - 12)),
        _ => Event::Unknown,
    };

    Ok(Some(InternalEvent::Input(input_event)))
}

fn parse_csi_rxvt_mouse(buffer: &[u8]) -> Result<Option<InternalEvent>> {
    // rxvt mouse encoding:
    // ESC [ Cb ; Cx ; Cy ; M

    assert!(buffer.starts_with(&[b'\x1B', b'['])); // ESC [
    assert!(buffer.ends_with(&[b'M']));

    let s = std::str::from_utf8(&buffer[2..buffer.len() - 1])
        .map_err(|_| could_not_parse_event_error())?;
    let mut split = s.split(';');

    let cb = next_parsed::<u16>(&mut split)?;
    let cx = next_parsed::<u16>(&mut split)? - 1;
    let cy = next_parsed::<u16>(&mut split)? - 1;

    let mouse_input_event = match cb {
        32 => MouseEvent::Press(MouseButton::Left, cx, cy),
        33 => MouseEvent::Press(MouseButton::Middle, cx, cy),
        34 => MouseEvent::Press(MouseButton::Right, cx, cy),
        35 => MouseEvent::Release(cx, cy),
        64 => MouseEvent::Hold(cx, cy),
        96 | 97 => MouseEvent::Press(MouseButton::WheelUp, cx, cy),
        _ => MouseEvent::Unknown,
    };

    Ok(Some(InternalEvent::Input(Event::Mouse(mouse_input_event))))
}

fn parse_csi_x10_mouse(buffer: &[u8]) -> Result<Option<InternalEvent>> {
    // X10 emulation mouse encoding: ESC [ M CB Cx Cy (6 characters only).
    // NOTE (@imdaveho): cannot find documentation on this

    assert!(buffer.starts_with(&[b'\x1B', b'[', b'M'])); // ESC [ M

    if buffer.len() < 6 {
        return Ok(None);
    }

    let cb = buffer[3] as i8 - 32;
    // See http://www.xfree86.org/current/ctlseqs.html#Mouse%20Tracking
    // The upper left character position on the terminal is denoted as 1,1.
    // Subtract 1 to keep it synced with cursor
    let cx = buffer[4].saturating_sub(32) as u16 - 1;
    let cy = buffer[5].saturating_sub(32) as u16 - 1;

    let mouse_input_event = match cb & 0b11 {
        0 => {
            if cb & 0x40 != 0 {
                MouseEvent::Press(MouseButton::WheelUp, cx, cy)
            } else {
                MouseEvent::Press(MouseButton::Left, cx, cy)
            }
        }
        1 => {
            if cb & 0x40 != 0 {
                MouseEvent::Press(MouseButton::WheelDown, cx, cy)
            } else {
                MouseEvent::Press(MouseButton::Middle, cx, cy)
            }
        }
        2 => MouseEvent::Press(MouseButton::Right, cx, cy),
        3 => MouseEvent::Release(cx, cy),
        _ => MouseEvent::Unknown,
    };

    Ok(Some(InternalEvent::Input(Event::Mouse(mouse_input_event))))
}

fn parse_csi_xterm_mouse(buffer: &[u8]) -> Result<Option<InternalEvent>> {
    // ESC [ < Cb ; Cx ; Cy (;) (M or m)

    assert!(buffer.starts_with(&[b'\x1B', b'[', b'<'])); // ESC [ <

    if !buffer.ends_with(&[b'm']) && !buffer.ends_with(&[b'M']) {
        return Ok(None);
    }

    let s = std::str::from_utf8(&buffer[3..buffer.len() - 1])
        .map_err(|_| could_not_parse_event_error())?;
    let mut split = s.split(';');

    let cb = next_parsed::<u16>(&mut split)?;

    // See http://www.xfree86.org/current/ctlseqs.html#Mouse%20Tracking
    // The upper left character position on the terminal is denoted as 1,1.
    // Subtract 1 to keep it synced with cursor
    let cx = next_parsed::<u16>(&mut split)? - 1;
    let cy = next_parsed::<u16>(&mut split)? - 1;

    let input_event = match cb {
        0..=2 | 64..=65 => {
            let button = match cb {
                0 => MouseButton::Left,
                1 => MouseButton::Middle,
                2 => MouseButton::Right,
                64 => MouseButton::WheelUp,
                65 => MouseButton::WheelDown,
                _ => unreachable!(),
            };
            match buffer.last().unwrap() {
                b'M' => Event::Mouse(MouseEvent::Press(button, cx, cy)),
                b'm' => Event::Mouse(MouseEvent::Release(cx, cy)),
                _ => Event::Unknown,
            }
        }
        // TODO 1.0: Add MouseButton to Hold and report which button is pressed
        // 33 - middle, 34 - right
        32 => Event::Mouse(MouseEvent::Hold(cx, cy)),
        3 => Event::Mouse(MouseEvent::Release(cx, cy)),
        _ => Event::Unknown,
    };

    Ok(Some(InternalEvent::Input(input_event)))
}

fn parse_utf8_char(buffer: &[u8]) -> Result<Option<char>> {
    match std::str::from_utf8(buffer) {
        Ok(s) => {
            let ch = s
                .chars()
                .next()
                .ok_or_else(|| could_not_parse_event_error())?;

            Ok(Some(ch))
        }
        Err(_) => {
            // from_utf8 failed, but we have to check if we need more bytes for code point
            // and if all the bytes we have no are valid

            let required_bytes = match buffer[0] {
                // https://en.wikipedia.org/wiki/UTF-8#Description
                (0x00..=0x7F) => 1, // 0xxxxxxx
                (0xC0..=0xDF) => 2, // 110xxxxx 10xxxxxx
                (0xE0..=0xEF) => 3, // 1110xxxx 10xxxxxx 10xxxxxx
                (0xF0..=0xF7) => 4, // 11110xxx 10xxxxxx 10xxxxxx 10xxxxxx
                (0x80..=0xBF) | (0xF8..=0xFF) => return Err(could_not_parse_event_error()),
            };

            // More than 1 byte, check them for 10xxxxxx pattern
            if required_bytes > 1 && buffer.len() > 1 {
                for byte in &buffer[1..] {
                    if byte & !0b0011_1111 != 0b1000_0000 {
                        return Err(could_not_parse_event_error());
                    }
                }
            }

            if buffer.len() < required_bytes {
                // All bytes looks good so far, but we need more of them
                Ok(None)
            } else {
                Err(could_not_parse_event_error())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_esc_key() {
        assert_eq!(
            parse_event("\x1B".as_bytes(), false).unwrap(),
            Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Esc))),
        );
    }

    #[test]
    fn test_possible_esc_sequence() {
        assert_eq!(parse_event("\x1B".as_bytes(), true).unwrap(), None,);
    }

    #[test]
    fn test_alt_key() {
        assert_eq!(
            parse_event("\x1Bc".as_bytes(), false).unwrap(),
            Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Alt('c')))),
        );
    }

    #[test]
    fn test_parse_event_subsequent_calls() {
        // The main purpose of this test is to check if we're passing
        // correct slice to other parse_ functions.

        // parse_csi_cursor_position
        assert_eq!(
            parse_event("\x1B[20;10R".as_bytes(), false).unwrap(),
            Some(InternalEvent::CursorPosition(9, 19))
        );

        // parse_csi
        assert_eq!(
            parse_event("\x1B[D".as_bytes(), false).unwrap(),
            Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Left))),
        );

        // parse_csi_modifier_key_code
        assert_eq!(
            parse_event("\x1B[2D".as_bytes(), false).unwrap(),
            Some(InternalEvent::Input(Event::Keyboard(KeyEvent::ShiftLeft))),
        );

        // parse_csi_special_key_code
        assert_eq!(
            parse_event("\x1B[3~".as_bytes(), false).unwrap(),
            Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Delete))),
        );

        // parse_csi_rxvt_mouse
        assert_eq!(
            parse_event("\x1B[32;30;40;M".as_bytes(), false).unwrap(),
            Some(InternalEvent::Input(Event::Mouse(MouseEvent::Press(
                MouseButton::Left,
                29,
                39
            ))))
        );

        // parse_csi_x10_mouse
        assert_eq!(
            parse_event("\x1B[M0\x60\x70".as_bytes(), false).unwrap(),
            Some(InternalEvent::Input(Event::Mouse(MouseEvent::Press(
                MouseButton::Left,
                63,
                79
            ))))
        );

        // parse_csi_xterm_mouse
        assert_eq!(
            parse_event("\x1B[<0;20;10;M".as_bytes(), false).unwrap(),
            Some(InternalEvent::Input(Event::Mouse(MouseEvent::Press(
                MouseButton::Left,
                19,
                9
            ))))
        );

        // parse_utf8_char
        assert_eq!(
            parse_event("Ž".as_bytes(), false).unwrap(),
            Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Char('Ž')))),
        );
    }

    #[test]
    fn test_parse_event() {
        assert_eq!(
            parse_event("\t".as_bytes(), false).unwrap(),
            Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Tab))),
        );
    }

    #[test]
    fn test_parse_csi_cursor_position() {
        assert_eq!(
            parse_csi_cursor_position("\x1B[20;10R".as_bytes()).unwrap(),
            Some(InternalEvent::CursorPosition(9, 19))
        );
    }

    #[test]
    fn test_parse_csi() {
        assert_eq!(
            parse_csi("\x1B[D".as_bytes()).unwrap(),
            Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Left))),
        );
    }

    #[test]
    fn test_parse_csi_modifier_key_code() {
        assert_eq!(
            parse_csi_modifier_key_code("\x1B[2D".as_bytes()).unwrap(),
            Some(InternalEvent::Input(Event::Keyboard(KeyEvent::ShiftLeft))),
        );
    }

    #[test]
    fn test_parse_csi_special_key_code() {
        assert_eq!(
            parse_csi_special_key_code("\x1B[3~".as_bytes()).unwrap(),
            Some(InternalEvent::Input(Event::Keyboard(KeyEvent::Delete))),
        );
    }

    #[test]
    fn test_parse_csi_special_key_code_multiple_values_not_supported() {
        assert_eq!(
            parse_csi_special_key_code("\x1B[3;2~".as_bytes()).unwrap(),
            Some(InternalEvent::Input(Event::Unknown)),
        );
    }

    #[test]
    fn test_parse_csi_rxvt_mouse() {
        assert_eq!(
            parse_csi_rxvt_mouse("\x1B[32;30;40;M".as_bytes()).unwrap(),
            Some(InternalEvent::Input(Event::Mouse(MouseEvent::Press(
                MouseButton::Left,
                29,
                39
            ))))
        );
    }

    #[test]
    fn test_parse_csi_x10_mouse() {
        assert_eq!(
            parse_csi_x10_mouse("\x1B[M0\x60\x70".as_bytes()).unwrap(),
            Some(InternalEvent::Input(Event::Mouse(MouseEvent::Press(
                MouseButton::Left,
                63,
                79
            ))))
        );
    }

    #[test]
    fn test_parse_csi_xterm_mouse() {
        assert_eq!(
            parse_csi_xterm_mouse("\x1B[<0;20;10;M".as_bytes()).unwrap(),
            Some(InternalEvent::Input(Event::Mouse(MouseEvent::Press(
                MouseButton::Left,
                19,
                9
            ))))
        );
        assert_eq!(
            parse_csi_xterm_mouse("\x1B[<0;20;10M".as_bytes()).unwrap(),
            Some(InternalEvent::Input(Event::Mouse(MouseEvent::Press(
                MouseButton::Left,
                19,
                9
            ))))
        );
        assert_eq!(
            parse_csi_xterm_mouse("\x1B[<0;20;10;m".as_bytes()).unwrap(),
            Some(InternalEvent::Input(Event::Mouse(MouseEvent::Release(
                19, 9
            ))))
        );
        assert_eq!(
            parse_csi_xterm_mouse("\x1B[<0;20;10m".as_bytes()).unwrap(),
            Some(InternalEvent::Input(Event::Mouse(MouseEvent::Release(
                19, 9
            ))))
        );
    }

    #[test]
    fn test_utf8() {
        // https://www.php.net/manual/en/reference.pcre.pattern.modifiers.php#54805

        // 'Valid ASCII' => "a",
        assert_eq!(parse_utf8_char("a".as_bytes()).unwrap(), Some('a'),);

        // 'Valid 2 Octet Sequence' => "\xc3\xb1",
        assert_eq!(parse_utf8_char(&[0xC3, 0xB1]).unwrap(), Some('ñ'),);

        // 'Invalid 2 Octet Sequence' => "\xc3\x28",
        assert!(parse_utf8_char(&[0xC3, 0x28]).is_err());

        // 'Invalid Sequence Identifier' => "\xa0\xa1",
        assert!(parse_utf8_char(&[0xA0, 0xA1]).is_err());

        // 'Valid 3 Octet Sequence' => "\xe2\x82\xa1",
        assert_eq!(
            parse_utf8_char(&[0xE2, 0x81, 0xA1]).unwrap(),
            Some('\u{2061}'),
        );

        // 'Invalid 3 Octet Sequence (in 2nd Octet)' => "\xe2\x28\xa1",
        assert!(parse_utf8_char(&[0xE2, 0x28, 0xA1]).is_err());

        // 'Invalid 3 Octet Sequence (in 3rd Octet)' => "\xe2\x82\x28",
        assert!(parse_utf8_char(&[0xE2, 0x82, 0x28]).is_err());

        // 'Valid 4 Octet Sequence' => "\xf0\x90\x8c\xbc",
        assert_eq!(
            parse_utf8_char(&[0xF0, 0x90, 0x8C, 0xBC]).unwrap(),
            Some('𐌼'),
        );

        // 'Invalid 4 Octet Sequence (in 2nd Octet)' => "\xf0\x28\x8c\xbc",
        assert!(parse_utf8_char(&[0xF0, 0x28, 0x8C, 0xBC]).is_err());

        // 'Invalid 4 Octet Sequence (in 3rd Octet)' => "\xf0\x90\x28\xbc",
        assert!(parse_utf8_char(&[0xF0, 0x90, 0x28, 0xBC]).is_err());

        // 'Invalid 4 Octet Sequence (in 4th Octet)' => "\xf0\x28\x8c\x28",
        assert!(parse_utf8_char(&[0xF0, 0x28, 0x8C, 0x28]).is_err());
    }
}