// SPDX-License-Identifier: MIT

//! The terminal engine behind Strata's embedded terminal pane: a shell on a
//! PTY, Ghostty's VT state machine, and a drawable [`Frame`] of the screen.
//! Drawing and input encoding are left to the caller.

use libghostty_vt::fmt::{Format, Formatter};

pub use libghostty_vt::Terminal;

mod error;
mod frame;
pub mod ghostty;
mod osc7;
mod pty;
mod session;
mod shell;

pub use error::Error;
pub use frame::{Frame, FrameCursor, FrameRow, Run, RunStyle};
pub use ghostty::GhosttyLook;
pub use libghostty_vt::color::{Palette, Rgb};
pub use libghostty_vt::render::RowId;
pub use libghostty_vt::style::Underline;
pub use libghostty_vt::terminal::CursorStyle;
pub use libghostty_vt::terminal::ScrollViewport;
pub use session::{DefaultColors, Events, Pumped, Scrollbar, Session};
pub use shell::{ShellOptions, TERM_FALLBACK, TERM_GHOSTTY};

#[cfg(test)]
mod tests;

/// The terminal grid and the pixel size of one cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
}

impl Size {
    fn at_least_one_cell(self) -> Self {
        Self {
            cols: self.cols.max(1),
            rows: self.rows.max(1),
            ..self
        }
    }
}

/// The libghostty-vt version Strata is linked against.
#[must_use]
pub fn engine_version() -> String {
    libghostty_vt::build_info::version_string()
        .to_string_lossy()
        .into_owned()
}

/// The visible screen as plain text, trailing whitespace trimmed.
///
/// # Errors
/// Fails when libghostty cannot format the terminal.
pub fn plain_text(terminal: &Terminal<'_, '_>) -> Result<String, libghostty_vt::Error> {
    let mut formatter = Formatter::new(terminal)?;
    formatter.set_format(Format::Plain)?.set_trim(true)?;
    let bytes = formatter.format_alloc(None)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
