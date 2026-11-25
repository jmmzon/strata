// SPDX-License-Identifier: MIT

//! A drawable copy of the visible screen, rebuilt row by row.
//!
//! Every row keeps the libghostty row identity and a version that changes only
//! when its content does, so a renderer can cache one node per row and reuse it
//! as the row scrolls.

use std::collections::HashMap;

use libghostty_vt::color::Rgb;
use libghostty_vt::render::{
    CellIteration, CellIterator, Colors, Dirty, RenderState, RowId, RowIteration, RowIterator,
};
use libghostty_vt::screen::CellWide;
use libghostty_vt::style::{StyleColor, Underline};
use libghostty_vt::terminal::CursorStyle;
use libghostty_vt::{Error, Terminal};

/// The visible screen.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub cols: u16,
    pub rows: Vec<FrameRow>,
    pub cursor: Option<FrameCursor>,
    pub foreground: Rgb,
    pub background: Rgb,
}

/// One screen row as runs of equally styled text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameRow {
    pub id: RowId,
    /// Changes whenever the row's content does; unique across the session.
    pub version: u64,
    pub runs: Vec<Run>,
}

/// Neighbouring ASCII cells that share one style, or one non-ASCII cell.
/// Blank cells with no background at the end of a row are left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub col: u16,
    /// Width in cells; a wide character counts two.
    pub cells: u16,
    pub text: String,
    pub style: RunStyle,
}

/// A cell style with colors resolved and `inverse` already applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunStyle {
    pub fg: Rgb,
    /// `None` is the terminal background.
    pub bg: Option<Rgb>,
    pub underline: Underline,
    pub underline_color: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub strikethrough: bool,
    pub overline: bool,
}

impl RunStyle {
    fn is_plain_blank(&self) -> bool {
        self.bg.is_none()
            && self.underline == Underline::None
            && !self.strikethrough
            && !self.overline
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameCursor {
    pub col: u16,
    pub row: u16,
    pub style: CursorStyle,
    pub blinking: bool,
    /// `None` draws the cursor in the foreground color.
    pub color: Option<Rgb>,
    /// The cursor sits on the right half of a wide character.
    pub at_wide_tail: bool,
}

/// The libghostty handles reused for every frame.
pub(crate) struct FrameBuilder {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    colors: Option<Colors>,
    next_version: u64,
    text: String,
}

impl FrameBuilder {
    pub(crate) fn new() -> Result<Self, Error> {
        Ok(Self {
            state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            colors: None,
            next_version: 0,
            text: String::new(),
        })
    }

    /// Brings `frame` up to date with `terminal`; returns whether anything
    /// changed.
    pub(crate) fn update(
        &mut self,
        terminal: &Terminal<'static, 'static>,
        frame: &mut Frame,
    ) -> Result<bool, Error> {
        let snapshot = self.state.update(terminal)?;
        let colors = snapshot.colors()?;
        let rows = usize::from(snapshot.rows()?);
        let full = snapshot.dirty()? == Dirty::Full
            || frame.rows.len() != rows
            || self.colors.as_ref() != Some(&colors);
        let cursor = cursor(&snapshot.cursor()?, &colors);
        let changed = full || snapshot.dirty()? != Dirty::Clean || frame.cursor != cursor;

        let mut old: HashMap<RowId, FrameRow> =
            frame.rows.drain(..).map(|row| (row.id, row)).collect();
        let mut iteration = self.rows.update(&snapshot)?;
        while let Some(row) = iteration.next() {
            let id = row.id()?;
            let previous = old.remove(&id);
            let row_frame = match previous {
                Some(previous) if !full && !row.dirty()? => previous,
                previous => {
                    let runs = build_runs(&mut self.cells, row, &colors, &mut self.text)?;
                    // Scrolling marks moved rows dirty although they are unchanged.
                    match previous {
                        Some(previous) if previous.runs == runs => previous,
                        _ => {
                            self.next_version += 1;
                            FrameRow {
                                id,
                                version: self.next_version,
                                runs,
                            }
                        }
                    }
                }
            };
            frame.rows.push(row_frame);
        }
        snapshot.clean()?;

        frame.cols = snapshot.cols()?;
        frame.cursor = cursor;
        frame.foreground = colors.foreground;
        frame.background = colors.background;
        self.colors = Some(colors);
        Ok(changed)
    }
}

fn cursor(cursor: &libghostty_vt::render::Cursor, colors: &Colors) -> Option<FrameCursor> {
    let viewport = cursor.viewport.filter(|_| cursor.visible)?;
    Some(FrameCursor {
        col: viewport.x,
        row: viewport.y,
        style: cursor.visual_style,
        blinking: cursor.blinking,
        color: colors.cursor,
        at_wide_tail: viewport.at_wide_tail,
    })
}

fn build_runs(
    cells: &mut CellIterator<'static>,
    row: &RowIteration<'static, '_>,
    colors: &Colors,
    text: &mut String,
) -> Result<Vec<Run>, Error> {
    let mut runs: Vec<Run> = Vec::new();
    let mut col = 0u16;
    let mut iteration = cells.update(row)?;
    while let Some(cell) = iteration.next() {
        let width = match cell.raw_cell()?.wide()? {
            CellWide::Narrow => 1,
            CellWide::Wide => 2,
            CellWide::SpacerTail | CellWide::SpacerHead => continue,
        };
        let (style, invisible) = cell_style(cell, colors)?;
        cell.graphemes_utf8(text)?;
        if text.is_empty() || invisible {
            text.clear();
            text.push(' ');
        }
        // Non-ASCII cells get runs of their own so a fallback font's advance
        // cannot push later cells off the grid.
        match runs.last_mut() {
            Some(run) if run.style == style && text.is_ascii() && run.text.is_ascii() => {
                run.cells += width;
                run.text.push_str(text);
            }
            _ => runs.push(Run {
                col,
                cells: width,
                text: text.clone(),
                style,
            }),
        }
        col += width;
    }
    trim_trailing_blanks(&mut runs);
    Ok(runs)
}

fn cell_style(
    cell: &CellIteration<'static, '_>,
    colors: &Colors,
) -> Result<(RunStyle, bool), Error> {
    let style = cell.style()?;
    let mut fg = cell.fg_color()?.unwrap_or(colors.foreground);
    let mut bg = cell.bg_color()?;
    if style.inverse {
        let swapped = bg.unwrap_or(colors.background);
        bg = Some(fg);
        fg = swapped;
    }
    let underline_color = match style.underline_color {
        StyleColor::None => None,
        StyleColor::Rgb(rgb) => Some(rgb),
        StyleColor::Palette(index) => Some(colors.palette[usize::from(index.0)]),
    };
    let run_style = RunStyle {
        fg,
        bg,
        underline: style.underline,
        underline_color,
        bold: style.bold,
        italic: style.italic,
        faint: style.faint,
        strikethrough: style.strikethrough,
        overline: style.overline,
    };
    Ok((run_style, style.invisible))
}

fn trim_trailing_blanks(runs: &mut Vec<Run>) {
    while let Some(run) = runs.last_mut() {
        if !run.style.is_plain_blank() {
            return;
        }
        let kept = run.text.trim_end_matches(' ').len();
        let trimmed = run.text.len() - kept;
        if kept > 0 {
            run.text.truncate(kept);
            run.cells -= u16::try_from(trimmed).unwrap_or(run.cells);
            return;
        }
        runs.pop();
    }
}
