use std::{
    io::{self, Error, ErrorKind, Write},
    time::Duration,
};

use crate::input::{enqueue_internal, poll_internal, read_internal};
use crate::{
    input::events::InternalEvent,
    utils::{
        sys::unix::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled},
        Result,
    },
};

/// Returns the cursor position (column, row).
///
/// The top left cell is represented `0,0`.
pub fn position() -> Result<(u16, u16)> {
    if is_raw_mode_enabled() {
        read_position_raw()
    } else {
        read_position()
    }
}

fn read_position() -> Result<(u16, u16)> {
    enable_raw_mode()?;
    let pos = read_position_raw();
    disable_raw_mode()?;
    pos
}

fn read_position_raw() -> Result<(u16, u16)> {
    // Use `ESC [ 6 n` to and retrieve the cursor position.
    let mut stdout = io::stdout();

    // Write command
    stdout.write_all(b"\x1B[6n")?;
    stdout.flush()?;

    // acquire mutable lock until we read the position, so that the user can't steal it from us.

    loop {
        match poll_internal(Some(Duration::from_millis(2000))) {
            Ok(true) => {
                match read_internal() {
                    Ok(InternalEvent::CursorPosition(x, y)) => {
                        return Ok((x, y));
                    }
                    Ok(event) => {
                        // We don't want to steal user events.
                        enqueue_internal(event);
                    }
                    _ => {}
                };
            }
            Ok(false) => {
                return Err(Error::new(
                    ErrorKind::Other,
                    "The cursor position could not be read within a normal duration",
                ))?;
            }
            Err(_) => {}
        }
    }
}
