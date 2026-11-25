// SPDX-License-Identifier: MIT

//! A shell on a PTY, fed through libghostty-vt.
//!
//! Everything runs on the caller's thread: libghostty handles are not `Send`.
//! The caller watches [`Session::fd`] and calls [`Session::pump`] when it is
//! readable and [`Session::flush`] when it is writable and
//! [`Session::wants_write`] holds.

use std::cell::{Cell, RefCell};
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::rc::Rc;

use libghostty_vt::Terminal;
use libghostty_vt::color::{Palette, Rgb};
use libghostty_vt::terminal::ScrollViewport;

use crate::frame::{Frame, FrameBuilder};
use crate::pty::Pty;
use crate::shell::ShellOptions;
use crate::{Error, Size};

/// The most one [`Session::pump`] reads before handing control back, so a
/// flood of output cannot starve the event loop.
const PUMP_LIMIT: usize = 256 * 1024;
const READ_CHUNK: usize = 64 * 1024;

/// What one [`Session::pump`] call did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pumped {
    pub bytes: usize,
    /// The read limit was hit and more output may be waiting.
    pub more: bool,
    /// The child closed the terminal.
    pub hung_up: bool,
}

/// Terminal events since the last [`Session::take_events`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Events {
    pub pwd_changed: bool,
    pub title_changed: bool,
    pub bell: bool,
}

/// Default colors; `None` keeps libghostty's own.
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultColors {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub palette: Option<Palette>,
}

/// The scrollable area and the viewport within it, in rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scrollbar {
    pub total: u64,
    pub offset: u64,
    pub visible: u64,
}

#[derive(Default)]
struct Shared {
    replies: RefCell<Vec<u8>>,
    pwd_changed: Cell<bool>,
    title_changed: Cell<bool>,
    bell: Cell<bool>,
}

pub struct Session {
    terminal: Terminal<'static, 'static>,
    pty: Pty,
    shared: Rc<Shared>,
    outgoing: Vec<u8>,
    builder: FrameBuilder,
    frame: Frame,
    size: Size,
    exit: Option<ExitStatus>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("pid", &self.pty.pid())
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Starts the shell on a new terminal of `size`.
    ///
    /// # Errors
    /// Fails when the PTY cannot be opened, the program cannot be started, or
    /// libghostty runs out of memory.
    pub fn spawn(options: &ShellOptions, size: Size) -> Result<Self, Error> {
        let size = size.at_least_one_cell();
        let mut terminal = Terminal::new(size.cols, size.rows)?;
        terminal.resize(
            size.cols,
            size.rows,
            size.cell_width_px,
            size.cell_height_px,
        )?;
        terminal.set_terminfo_name(options.term())?;
        let shared = Rc::new(Shared::default());
        connect(&mut terminal, &shared)?;
        let pty = Pty::spawn(options.command(), size)?;
        Ok(Self {
            terminal,
            pty,
            shared,
            outgoing: Vec::new(),
            builder: FrameBuilder::new()?,
            frame: Frame::default(),
            size,
            exit: None,
        })
    }

    /// The PTY master to watch for input and, while [`Self::wants_write`]
    /// holds, for room to write.
    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.pty.fd()
    }

    /// A duplicate of the PTY master for an event loop to poll; reading and
    /// writing still go through the session.
    ///
    /// # Errors
    /// Fails when the descriptor cannot be duplicated.
    pub fn poll_fd(&self) -> io::Result<OwnedFd> {
        self.pty.fd().try_clone_to_owned()
    }

    /// Feeds waiting output into the terminal, then sends its replies.
    ///
    /// # Errors
    /// Fails on a read or write error other than "try again".
    pub fn pump(&mut self) -> io::Result<Pumped> {
        let mut pumped = Pumped::default();
        let mut buf = vec![0; READ_CHUNK];
        while pumped.bytes < PUMP_LIMIT {
            match self.pty.read(&mut buf) {
                Ok(0) => {
                    pumped.hung_up = true;
                    break;
                }
                Ok(n) => {
                    self.terminal.vt_write(&buf[..n]);
                    pumped.bytes += n;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        pumped.more = pumped.bytes >= PUMP_LIMIT;
        self.queue_replies();
        self.flush()?;
        Ok(pumped)
    }

    /// Queues bytes for the shell, as typed or pasted (already encoded).
    ///
    /// # Errors
    /// Fails on a write error other than "try again".
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.outgoing.extend_from_slice(data);
        self.flush()
    }

    /// Writes as much queued input as the PTY takes now.
    ///
    /// # Errors
    /// Fails on a write error other than "try again".
    pub fn flush(&mut self) -> io::Result<()> {
        let mut written = 0;
        while written < self.outgoing.len() {
            match self.pty.write(&self.outgoing[written..]) {
                Ok(n) => written += n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.outgoing.drain(..written);
                    return Err(error);
                }
            }
        }
        self.outgoing.drain(..written);
        Ok(())
    }

    /// Input is queued that the PTY has not taken yet.
    #[must_use]
    pub fn wants_write(&self) -> bool {
        !self.outgoing.is_empty()
    }

    /// Resizes the terminal and tells the shell.
    ///
    /// # Errors
    /// Fails when libghostty or the PTY rejects the size.
    pub fn resize(&mut self, size: Size) -> Result<(), Error> {
        let size = size.at_least_one_cell();
        if size == self.size {
            return Ok(());
        }
        self.terminal.resize(
            size.cols,
            size.rows,
            size.cell_width_px,
            size.cell_height_px,
        )?;
        self.pty.resize(size)?;
        self.size = size;
        Ok(())
    }

    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    /// Brings the frame up to date; the flag says whether it changed.
    ///
    /// # Errors
    /// Fails when libghostty runs out of memory.
    pub fn frame(&mut self) -> Result<(&Frame, bool), Error> {
        let changed = self.builder.update(&self.terminal, &mut self.frame)?;
        Ok((&self.frame, changed))
    }

    /// Sets the colors programs get until they change them themselves.
    ///
    /// # Errors
    /// Fails when libghostty rejects a value.
    pub fn set_default_colors(&mut self, colors: &DefaultColors) -> Result<(), Error> {
        self.terminal
            .set_default_fg_color(colors.foreground)?
            .set_default_bg_color(colors.background)?
            .set_default_cursor_color(colors.cursor)?
            .set_default_color_palette(colors.palette)?;
        Ok(())
    }

    /// Moves the viewport through the scrollback.
    ///
    /// # Errors
    /// Fails when libghostty rejects the request.
    pub fn scroll(&mut self, scroll: ScrollViewport) -> Result<(), Error> {
        Ok(self.terminal.scroll_viewport(scroll)?)
    }

    /// Where the viewport is within the scrollback, in rows.
    #[must_use]
    pub fn scrollbar(&self) -> Scrollbar {
        self.terminal
            .scrollbar()
            .map(|bar| Scrollbar {
                total: bar.total,
                offset: bar.offset,
                visible: bar.len,
            })
            .unwrap_or_default()
    }

    pub fn take_events(&mut self) -> Events {
        Events {
            pwd_changed: self.shared.pwd_changed.take(),
            title_changed: self.shared.title_changed.take(),
            bell: self.shared.bell.take(),
        }
    }

    /// The shell's directory: from OSC 7 when the shell reports it, else
    /// from `/proc`.
    #[must_use]
    pub fn cwd(&self) -> Option<PathBuf> {
        self.terminal
            .pwd()
            .ok()
            .and_then(crate::osc7::path)
            .or_else(|| self.pty.shell_cwd())
    }

    #[must_use]
    pub fn title(&self) -> String {
        self.terminal
            .title()
            .map(|title| String::from_utf8_lossy(title).into_owned())
            .unwrap_or_default()
    }

    /// The shell's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pty.pid()
    }

    /// A program other than the shell owns the terminal.
    #[must_use]
    pub fn has_foreground_job(&self) -> bool {
        self.pty.has_foreground_job()
    }

    /// The cursor is on a shell prompt (needs OSC 133 from the shell).
    #[must_use]
    pub fn is_at_prompt(&self) -> bool {
        self.terminal.is_cursor_at_prompt().unwrap_or(false)
    }

    /// The shell's exit status, once it has exited.
    pub fn exit_status(&mut self) -> Option<ExitStatus> {
        if self.exit.is_none() {
            self.exit = self.pty.try_wait().ok().flatten();
        }
        self.exit
    }

    /// The libghostty terminal, for the encoders and the plain-text view.
    #[must_use]
    pub fn terminal(&self) -> &Terminal<'static, 'static> {
        &self.terminal
    }

    fn queue_replies(&mut self) {
        let mut replies = self.shared.replies.borrow_mut();
        self.outgoing.append(&mut replies);
    }
}

fn connect(terminal: &mut Terminal<'static, 'static>, shared: &Rc<Shared>) -> Result<(), Error> {
    let replies = Rc::clone(shared);
    terminal.on_pty_write(move |_, data| replies.replies.borrow_mut().extend_from_slice(data))?;
    let pwd = Rc::clone(shared);
    terminal.on_pwd_changed(move |_| pwd.pwd_changed.set(true))?;
    let title = Rc::clone(shared);
    terminal.on_title_changed(move |_| title.title_changed.set(true))?;
    let bell = Rc::clone(shared);
    terminal.on_bell(move |_| bell.bell.set(true))?;
    Ok(())
}
