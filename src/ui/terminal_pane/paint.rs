// SPDX-License-Identifier: MIT

//! Turns a terminal frame into render nodes: one cached node per row, plus
//! the cursor drawn on top.

use gtk::{gdk, graphene, gsk, pango, prelude::*};
use strata_term::{CursorStyle, Frame, FrameCursor, FrameRow, Rgb, Run, RunStyle, Underline};

const FAINT_ALPHA: f32 = 0.5;
const BAR_WIDTH: f32 = 2.0;
const UNDERLINE_HEIGHT: f32 = 2.0;
const HOLLOW_WIDTH: f32 = 1.0;

/// The pixel size of one cell, and the spacing that keeps glyphs on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Metrics {
    pub(super) cell_width: f32,
    pub(super) cell_height: f32,
    /// Pango units added after each glyph so its advance matches the cell.
    letter_spacing: i32,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            cell_width: 8.0,
            cell_height: 16.0,
            letter_spacing: 0,
        }
    }
}

/// Cells are whole pixels wide and tall, so rows and columns never blur.
pub(super) fn metrics(context: &pango::Context, font: &pango::FontDescription) -> Metrics {
    let sample = "0".repeat(10);
    let layout = pango::Layout::new(context);
    layout.set_font_description(Some(font));
    layout.set_text(&sample);
    let (_, logical) = layout.extents();
    let advance = f64::from(logical.width()) / 10.0;
    let cell_width = (advance / f64::from(pango::SCALE)).round().max(1.0);
    let cell_height = (f64::from(logical.height()) / f64::from(pango::SCALE))
        .ceil()
        .max(1.0);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a glyph advance in Pango units fits i32"
    )]
    let letter_spacing = (cell_width * f64::from(pango::SCALE) - advance).round() as i32;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "cell sizes are a few dozen pixels"
    )]
    Metrics {
        cell_width: cell_width as f32,
        cell_height: cell_height as f32,
        letter_spacing,
    }
}

/// What one row is drawn with.
pub(super) struct Painter<'a> {
    pub(super) context: &'a pango::Context,
    pub(super) font: &'a pango::FontDescription,
    pub(super) metrics: Metrics,
}

impl Painter<'_> {
    /// A row's backgrounds and text, at the origin.
    pub(super) fn row(&self, row: &FrameRow) -> Option<gsk::RenderNode> {
        let snapshot = gtk::Snapshot::new();
        for run in &row.runs {
            self.background(&snapshot, run);
        }
        for run in &row.runs {
            self.text(
                &snapshot,
                run.col,
                &run.text,
                &run.style,
                rgba(run.style.fg, alpha(&run.style)),
            );
        }
        snapshot.to_node()
    }

    fn background(&self, snapshot: &gtk::Snapshot, run: &Run) {
        let Some(bg) = run.style.bg else {
            return;
        };
        let rect = graphene::Rect::new(
            f32::from(run.col) * self.metrics.cell_width,
            0.0,
            f32::from(run.cells) * self.metrics.cell_width,
            self.metrics.cell_height,
        );
        snapshot.append_color(&rgba(bg, 1.0), &rect);
    }

    fn text(
        &self,
        snapshot: &gtk::Snapshot,
        col: u16,
        text: &str,
        style: &RunStyle,
        color: gdk::RGBA,
    ) {
        if text.trim_end_matches(' ').is_empty()
            && style.underline == Underline::None
            && !style.strikethrough
        {
            return;
        }
        let layout = pango::Layout::new(self.context);
        layout.set_font_description(Some(&self.font_for(style)));
        layout.set_attributes(Some(&self.attributes(style)));
        layout.set_text(text);
        snapshot.save();
        snapshot.translate(&graphene::Point::new(
            f32::from(col) * self.metrics.cell_width,
            0.0,
        ));
        snapshot.append_layout(&layout, &color);
        snapshot.restore();
    }

    fn font_for(&self, style: &RunStyle) -> pango::FontDescription {
        let mut font = self.font.clone();
        if style.bold {
            font.set_weight(pango::Weight::Bold);
        }
        if style.italic {
            font.set_style(pango::Style::Italic);
        }
        font
    }

    fn attributes(&self, style: &RunStyle) -> pango::AttrList {
        let attributes = pango::AttrList::new();
        attributes.insert(pango::AttrInt::new_letter_spacing(
            self.metrics.letter_spacing,
        ));
        let underline = match style.underline {
            Underline::None => None,
            Underline::Double => Some(pango::Underline::Double),
            Underline::Curly => Some(pango::Underline::Error),
            _ => Some(pango::Underline::Single),
        };
        if let Some(underline) = underline {
            attributes.insert(pango::AttrInt::new_underline(underline));
            if let Some(color) = style.underline_color {
                attributes.insert(pango::AttrColor::new_underline_color(
                    channel(color.r),
                    channel(color.g),
                    channel(color.b),
                ));
            }
        }
        if style.strikethrough {
            attributes.insert(pango::AttrInt::new_strikethrough(true));
        }
        attributes
    }

    /// The cursor over the drawn rows. A block shows the cell beneath it in
    /// the background color; without focus it is an outline.
    pub(super) fn cursor(
        &self,
        snapshot: &gtk::Snapshot,
        frame: &Frame,
        cursor: &FrameCursor,
        focused: bool,
    ) {
        let color = rgba(cursor.color.unwrap_or(frame.foreground), 1.0);
        let col = if cursor.at_wide_tail {
            cursor.col.saturating_sub(1)
        } else {
            cursor.col
        };
        let cell = frame
            .rows
            .get(usize::from(cursor.row))
            .and_then(|row| cell_at(row, col));
        let cells = cell.as_ref().map_or(1, |(_, _, width)| *width);
        let (x, y) = (
            f32::from(col) * self.metrics.cell_width,
            f32::from(cursor.row) * self.metrics.cell_height,
        );
        let width = f32::from(cells) * self.metrics.cell_width;
        let height = self.metrics.cell_height;
        let rect = |x: f32, y: f32, w: f32, h: f32| {
            snapshot.append_color(&color, &graphene::Rect::new(x, y, w, h))
        };
        match (cursor.style, focused) {
            (CursorStyle::Bar, _) => rect(x, y, BAR_WIDTH, height),
            (CursorStyle::Underline, _) => {
                rect(x, y + height - UNDERLINE_HEIGHT, width, UNDERLINE_HEIGHT)
            }
            (CursorStyle::Block, true) => {
                rect(x, y, width, height);
                if let Some((text, style, _)) = cell {
                    snapshot.save();
                    snapshot.translate(&graphene::Point::new(0.0, y));
                    self.text(snapshot, col, &text, &style, rgba(frame.background, 1.0));
                    snapshot.restore();
                }
            }
            _ => {
                rect(x, y, width, HOLLOW_WIDTH);
                rect(x, y + height - HOLLOW_WIDTH, width, HOLLOW_WIDTH);
                rect(x, y, HOLLOW_WIDTH, height);
                rect(x + width - HOLLOW_WIDTH, y, HOLLOW_WIDTH, height);
            }
        }
    }
}

/// The text, style, and width of the cell at `col`.
fn cell_at(row: &FrameRow, col: u16) -> Option<(String, RunStyle, u16)> {
    let run = row
        .runs
        .iter()
        .find(|run| (run.col..run.col + run.cells).contains(&col))?;
    if !run.text.is_ascii() {
        return Some((run.text.clone(), run.style, run.cells));
    }
    let index = usize::from(col - run.col);
    let text = run.text.get(index..=index)?.to_owned();
    Some((text, run.style, 1))
}

fn alpha(style: &RunStyle) -> f32 {
    if style.faint { FAINT_ALPHA } else { 1.0 }
}

pub(super) fn rgba(color: Rgb, alpha: f32) -> gdk::RGBA {
    gdk::RGBA::new(
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        alpha,
    )
}

fn channel(value: u8) -> u16 {
    u16::from(value) * 257
}
