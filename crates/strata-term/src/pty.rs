// SPDX-License-Identifier: MIT

//! A pseudo-terminal with a shell on its other end.
//!
//! The master side is non-blocking, so the caller reads and writes it from its
//! event loop (Strata: a GLib fd watch on the GTK main thread) and never waits.

use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{Pid, Signal};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{Winsize, tcgetpgrp, tcsetwinsize};

use crate::Size;

/// The PTY master and the process started on it.
#[derive(Debug)]
pub(crate) struct Pty {
    master: OwnedFd,
    child: Option<Child>,
    pid: Pid,
}

impl Pty {
    /// Starts `command` as the leader of a new session whose controlling
    /// terminal is a fresh PTY of `size`.
    pub(crate) fn spawn(mut command: Command, size: Size) -> io::Result<Self> {
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
        grantpt(&master)?;
        unlockpt(&master)?;
        tcsetwinsize(&master, winsize(size))?;
        let name = ptsname(&master, Vec::new())?;
        let slave = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;

        command
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        #[expect(
            unsafe_code,
            reason = "the child must take the PTY as its controlling terminal between fork and exec"
        )]
        // SAFETY: the hook runs in the forked child before exec, where only
        // async-signal-safe calls are allowed. setsid and the TIOCSCTTY ioctl
        // are plain system calls through rustix: no allocation, no locks.
        // std has already made the PTY slave the child's stdin.
        unsafe {
            command.pre_exec(|| {
                rustix::process::setsid()?;
                rustix::process::ioctl_tiocsctty(rustix::stdio::stdin())?;
                Ok(())
            });
        }
        let child = command.spawn()?;
        // The child's stdio handles were moved into `command`; drop them so the
        // only open slave descriptors left are the child's.
        drop(command);

        let flags = fcntl_getfl(&master)?;
        fcntl_setfl(&master, flags | OFlags::NONBLOCK)?;
        let pid = Pid::from_child(&child);
        Ok(Self {
            master,
            child: Some(child),
            pid,
        })
    }

    pub(crate) fn fd(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    /// Reads what the child wrote. `Ok(0)` means the child is gone: Linux
    /// reports EIO on the master once the last slave descriptor closes.
    pub(crate) fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match rustix::io::read(&self.master, buf) {
            Ok(n) => Ok(n),
            Err(rustix::io::Errno::IO) => Ok(0),
            Err(errno) => Err(errno.into()),
        }
    }

    pub(crate) fn write(&self, data: &[u8]) -> io::Result<usize> {
        Ok(rustix::io::write(&self.master, data)?)
    }

    pub(crate) fn resize(&self, size: Size) -> io::Result<()> {
        // The kernel sends SIGWINCH to the foreground process group.
        Ok(tcsetwinsize(&self.master, winsize(size))?)
    }

    pub(crate) fn pid(&self) -> u32 {
        self.pid.as_raw_nonzero().get().unsigned_abs()
    }

    /// Whether a job other than the shell owns the terminal right now
    /// (`vim`, `less`, a running build).
    pub(crate) fn has_foreground_job(&self) -> bool {
        tcgetpgrp(&self.master).is_ok_and(|group| group != self.pid)
    }

    /// The shell's working directory as the kernel sees it.
    pub(crate) fn shell_cwd(&self) -> Option<PathBuf> {
        std::fs::read_link(Path::new("/proc").join(self.pid.to_string()).join("cwd")).ok()
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match &mut self.child {
            Some(child) => child.try_wait(),
            None => Ok(None),
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Hang up the whole session, as closing a terminal window does.
        let _ = rustix::process::kill_process_group(self.pid, Signal::HUP);
        if let Some(mut child) = self.child.take()
            && !matches!(child.try_wait(), Ok(Some(_)))
        {
            // Reap it off the main thread so it never lingers as a zombie.
            let _ = std::thread::Builder::new()
                .name("strata-term-reap".into())
                .spawn(move || child.wait());
        }
    }
}

fn winsize(size: Size) -> Winsize {
    Winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: clamp_px(u32::from(size.cols) * size.cell_width_px),
        ws_ypixel: clamp_px(u32::from(size.rows) * size.cell_height_px),
    }
}

fn clamp_px(px: u32) -> u16 {
    u16::try_from(px).unwrap_or(u16::MAX)
}

/// The program's file name, for picking the shell integration.
pub(crate) fn program_name(program: &OsStr) -> &OsStr {
    Path::new(program).file_name().unwrap_or(program)
}
