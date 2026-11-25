// SPDX-License-Identifier: MIT

use std::{path::Path, time::Duration};

use gtk::{glib, prelude::*};
use strata_term::ShellOptions;

use super::view::TerminalView;

fn sh(script: &str) -> ShellOptions {
    ShellOptions {
        program: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        ..ShellOptions::default()
    }
}

/// Runs the main loop until `done` holds, for at most ten seconds.
fn spin_until(done: impl Fn() -> bool) -> bool {
    let context = glib::MainContext::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !done() && std::time::Instant::now() < deadline {
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

#[test]
fn draws_the_shell_output_and_hangs_up_the_shell_when_destroyed() {
    crate::test_support::gtk_test(
        "draws_the_shell_output_and_hangs_up_the_shell_when_destroyed",
        || {
            let adjustment = gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 0.0, 0.0);
            let view = TerminalView::new(sh("printf 'hello pane'; sleep 30"), &adjustment);
            let window = gtk::Window::new();
            window.set_default_size(400, 200);
            window.set_child(Some(&view));
            crate::test_support::present_window(&window);

            assert!(spin_until(|| view
                .screen_text()
                .is_some_and(|text| text.contains("hello pane"))));
            let clock = window.frame_clock().expect("frame clock");
            assert!(crate::test_support::wait_for_paint(&clock));
            assert!(view.cached_rows() > 0, "no row was drawn");
            let pid = view.shell_pid().expect("shell pid");

            window.destroy();
            drop(view);
            let proc_dir = Path::new("/proc").join(pid.to_string());
            assert!(
                spin_until(|| !proc_dir.exists() || is_zombie(&proc_dir)),
                "the shell outlived its pane"
            );
        },
    );
}

fn is_zombie(proc_dir: &Path) -> bool {
    std::fs::read_to_string(proc_dir.join("stat")).is_ok_and(|stat| {
        stat.rsplit(')')
            .next()
            .is_some_and(|rest| rest.trim_start().starts_with('Z'))
    })
}
