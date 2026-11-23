//! Mouse capture: off unless asked for, because a terminal that reports the
//! mouse to a program no longer selects text with it.
//!
//! Only button presses and the wheel are requested (mode 1000, in the SGR
//! encoding, mode 1006) — not motion, which would redraw the dashboard for
//! every cell the pointer crosses and is used by nothing here.

use std::io::{self, Write};

/// Asks the terminal to report clicks and the wheel.
pub const ON: &str = "\x1b[?1000h\x1b[?1006h";
/// Hands the mouse back to the terminal.
pub const OFF: &str = "\x1b[?1006l\x1b[?1000l";

/// Turns reporting on or off. Never prints on failure: the terminal may be
/// gone, and that is the caller's to decide about.
pub fn capture(on: bool) -> io::Result<()> {
    let mut out = io::stdout().lock();
    out.write_all(if on { ON } else { OFF }.as_bytes())?;
    out.flush()
}
