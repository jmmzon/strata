// SPDX-License-Identifier: MIT

use std::fmt;
use std::io;

/// A terminal session failure.
#[derive(Debug)]
pub enum Error {
    /// The PTY or the child process.
    Io(io::Error),
    /// libghostty-vt, which fails only when it runs out of memory or is
    /// handed a value it does not know.
    Engine(libghostty_vt::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "terminal I/O failed: {error}"),
            Self::Engine(error) => write!(f, "terminal engine failed: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Engine(error) => Some(error),
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<libghostty_vt::Error> for Error {
    fn from(error: libghostty_vt::Error) -> Self {
        Self::Engine(error)
    }
}
