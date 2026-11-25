// SPDX-License-Identifier: MIT

//! The terminal engine behind Strata's embedded terminal pane: Ghostty's VT
//! state machine, with the PTY and drawing left to the caller.

use libghostty_vt::Terminal;
use libghostty_vt::fmt::{Format, Formatter};

pub use libghostty_vt::error::Error;

#[cfg(test)]
mod tests;

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
pub fn plain_text(terminal: &Terminal<'_, '_>) -> Result<String, Error> {
    let mut formatter = Formatter::new(terminal)?;
    formatter.set_format(Format::Plain)?.set_trim(true)?;
    let bytes = formatter.format_alloc(None)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
