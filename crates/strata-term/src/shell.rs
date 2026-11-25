// SPDX-License-Identifier: MIT

//! What to start in the terminal, and the environment it gets.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::pty::program_name;

/// The terminfo entry Strata's terminal answers to when it is installed.
pub const TERM_GHOSTTY: &str = "xterm-ghostty";
/// The fallback when the `xterm-ghostty` entry cannot be found.
pub const TERM_FALLBACK: &str = "xterm-256color";
/// Ghostty integration features turned on for the shell. `ssh-env` makes
/// `ssh` send `TERM=xterm-256color` to hosts that lack the Ghostty entry.
const SHELL_FEATURES: &str = "cursor,sudo,title,ssh-env";

/// The command line, directory, and resources for a terminal session.
#[derive(Clone, Debug, Default)]
pub struct ShellOptions {
    /// The program to run; `$SHELL`, then `/bin/sh`, when unset.
    pub program: Option<OsString>,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Ghostty's `share/ghostty` directory, which holds `shell-integration`.
    pub ghostty_resources: Option<PathBuf>,
    /// A terminfo directory holding `x/xterm-ghostty`.
    pub terminfo: Option<PathBuf>,
    /// Extra variables, applied last.
    pub env: Vec<(OsString, OsString)>,
}

impl ShellOptions {
    /// Options that use the installed Ghostty's shell integration and
    /// terminfo, when Ghostty is found.
    #[must_use]
    pub fn with_installed_ghostty() -> Self {
        let ghostty_resources = crate::ghostty::resources_dir();
        let terminfo = ghostty_resources
            .as_deref()
            .and_then(crate::ghostty::terminfo_dir);
        Self {
            ghostty_resources,
            terminfo,
            ..Self::default()
        }
    }

    /// The `TERM` the session will report.
    #[must_use]
    pub fn term(&self) -> &'static str {
        if self.ghostty_terminfo().is_some() {
            TERM_GHOSTTY
        } else {
            TERM_FALLBACK
        }
    }

    fn ghostty_terminfo(&self) -> Option<&Path> {
        self.terminfo
            .as_deref()
            .filter(|dir| dir.join("x").join(TERM_GHOSTTY).is_file())
    }

    fn program(&self) -> OsString {
        self.program
            .clone()
            .or_else(|| std::env::var_os("SHELL").filter(|shell| !shell.is_empty()))
            .unwrap_or_else(|| "/bin/sh".into())
    }

    pub(crate) fn command(&self) -> Command {
        let program = self.program();
        let mut command = Command::new(&program);
        command.args(&self.args);
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        command
            .env("TERM", self.term())
            .env("COLORTERM", "truecolor")
            .env("TERM_PROGRAM", "strata")
            .env_remove("TERMINFO")
            .env_remove("LINES")
            .env_remove("COLUMNS");
        if let Some(terminfo) = self.ghostty_terminfo() {
            command.env("TERMINFO", terminfo);
        }
        if is_fish(&program)
            && let Some(resources) = &self.ghostty_resources
        {
            add_fish_integration(&mut command, resources);
        }
        command.envs(self.env.iter().map(|(key, value)| (key, value)));
        command
    }
}

fn is_fish(program: &OsStr) -> bool {
    program_name(program) == "fish"
}

/// Ghostty's fish integration loads from a `vendor_conf.d` that fish finds
/// through `XDG_DATA_DIRS`, and removes itself from that list again.
fn add_fish_integration(command: &mut Command, resources: &Path) {
    let dir = resources.join("shell-integration");
    if !dir.join("fish").is_dir() {
        return;
    }
    let mut data_dirs = vec![dir.clone()];
    match std::env::var_os("XDG_DATA_DIRS").filter(|dirs| !dirs.is_empty()) {
        Some(dirs) => data_dirs.extend(std::env::split_paths(&dirs)),
        None => data_dirs.extend(["/usr/local/share".into(), "/usr/share".into()]),
    }
    if let Ok(joined) = std::env::join_paths(data_dirs) {
        command
            .env("XDG_DATA_DIRS", joined)
            .env("GHOSTTY_SHELL_INTEGRATION_XDG_DIR", &dir)
            .env("GHOSTTY_RESOURCES_DIR", resources)
            .env("GHOSTTY_SHELL_FEATURES", SHELL_FEATURES);
    }
}
