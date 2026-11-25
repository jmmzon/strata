// SPDX-License-Identifier: MIT

//! The embedded terminal pane (erebor patch): a shell under the file view,
//! drawn by Strata from libghostty-vt's screen state.
//!
//! Until the F4 wiring lands it is mounted only when `STRATA_TERMINAL_PANE`
//! is set.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
};

use gtk::{glib, prelude::*};
use strata_term::{DefaultColors, Rgb, ShellOptions};

use crate::ui::theme::ThemeTokens;

mod paint;
#[cfg(test)]
mod tests;
mod view;

use view::{Look, TerminalView};

thread_local! {
    static THEME: Cell<Option<ThemeColors>> = const { Cell::new(None) };
    static VIEWS: RefCell<Vec<glib::WeakRef<TerminalView>>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug)]
struct ThemeColors {
    foreground: Rgb,
    background: Rgb,
}

pub(crate) fn enabled() -> bool {
    std::env::var_os("STRATA_TERMINAL_PANE").is_some_and(|value| !value.is_empty() && value != "0")
}

/// A terminal with its scrollbar. The shell starts in `cwd` once the pane is
/// first laid out.
pub(crate) fn pane(cwd: Option<PathBuf>) -> gtk::Box {
    let adjustment = gtk::Adjustment::new(0.0, 0.0, 0.0, 1.0, 0.0, 0.0);
    let view = TerminalView::new(ShellOptions::with_installed_ghostty(), &adjustment);
    view.set_cwd(cwd);
    view.set_look(current_look());
    VIEWS.with_borrow_mut(|views| {
        views.retain(|view| view.upgrade().is_some());
        views.push(view.downgrade());
    });
    let scrollbar = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&adjustment));
    scrollbar.set_visible(false);
    let shown = scrollbar.downgrade();
    adjustment.connect_changed(move |adjustment| {
        if let Some(scrollbar) = shown.upgrade() {
            scrollbar.set_visible(adjustment.upper() > adjustment.page_size());
        }
    });
    let pane = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    pane.add_css_class("terminal-pane");
    pane.append(&view);
    pane.append(&scrollbar);
    pane
}

/// Follows Strata's theme: its text and background become the terminal's
/// default colors, and the palette is read again from the Ghostty config.
pub(in crate::ui) fn apply_theme(tokens: &ThemeTokens) {
    let parse = |value: &str| value.parse::<Rgb>().ok();
    let (Some(foreground), Some(background)) = (parse(&tokens.text), parse(&tokens.background))
    else {
        return;
    };
    THEME.set(Some(ThemeColors {
        foreground,
        background,
    }));
    let look = current_look();
    VIEWS.with_borrow_mut(|views| {
        views.retain(|view| {
            let Some(view) = view.upgrade() else {
                return false;
            };
            view.set_look(look.clone());
            true
        });
    });
}

fn current_look() -> Look {
    let theme = THEME.get();
    let dark = theme.is_none_or(|theme| theme.background.luminance() < 0.5);
    let ghostty = strata_term::ghostty::config_dir()
        .map(|dir| {
            let resources = strata_term::ghostty::resources_dir();
            strata_term::ghostty::read_look(&dir, resources.as_deref(), dark)
        })
        .unwrap_or_default();
    Look {
        font_family: ghostty.font_family,
        font_points: ghostty.font_size,
        colors: DefaultColors {
            foreground: theme.map(|theme| theme.foreground),
            background: theme.map(|theme| theme.background),
            cursor: None,
            palette: ghostty.palette,
        },
    }
}
