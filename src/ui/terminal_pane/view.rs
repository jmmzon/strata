// SPDX-License-Identifier: MIT

//! The widget that runs one shell and draws its screen.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
};

use gtk::{gio, glib, graphene, gsk, pango, prelude::*, subclass::prelude::*};
use strata_term::{DefaultColors, RowId, ScrollViewport, Session, ShellOptions, Size};

use super::paint::{self, Metrics, Painter};

const DEFAULT_FONT: &str = "monospace";
const DEFAULT_FONT_POINTS: f64 = 11.0;
/// Lines one wheel notch scrolls.
const WHEEL_LINES: f64 = 3.0;

/// What the widget draws with: the font and the colors programs start with.
#[derive(Clone, Debug, Default)]
pub(crate) struct Look {
    pub(crate) font_family: Option<String>,
    pub(crate) font_points: Option<f64>,
    pub(crate) colors: DefaultColors,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TerminalView {
        pub(super) session: RefCell<Option<Session>>,
        pub(super) read_watch: RefCell<Option<glib::SourceId>>,
        pub(super) write_watch: RefCell<Option<glib::SourceId>>,
        pub(super) exited: Cell<bool>,
        pub(super) options: RefCell<ShellOptions>,
        pub(super) look: RefCell<Look>,
        pub(super) font: RefCell<Option<pango::FontDescription>>,
        pub(super) metrics: Cell<Metrics>,
        pub(super) rows: RefCell<HashMap<RowId, (u64, gsk::RenderNode)>>,
        pub(super) adjustment: RefCell<Option<gtk::Adjustment>>,
        pub(super) syncing_adjustment: Cell<bool>,
        pub(super) scroll_rest: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TerminalView {
        const NAME: &'static str = "StrataTerminalView";
        type Type = super::TerminalView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            // GTK's Terminal role needs 4.14; Strata supports 4.12.
            klass.set_accessible_role(gtk::AccessibleRole::Generic);
            klass.set_css_name("terminal-view");
        }
    }

    impl ObjectImpl for TerminalView {
        fn constructed(&self) {
            self.parent_constructed();
            let view = self.obj();
            view.set_focusable(true);
            view.set_hexpand(true);
            view.set_vexpand(true);
            view.set_overflow(gtk::Overflow::Hidden);
            view.add_controllers();
        }

        fn dispose(&self) {
            self.obj().stop();
        }
    }

    impl WidgetImpl for TerminalView {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().fit(width, height);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().paint(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct TerminalView(ObjectSubclass<imp::TerminalView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TerminalView {
    pub(crate) fn new(options: ShellOptions, adjustment: &gtk::Adjustment) -> Self {
        let view: Self = glib::Object::new();
        view.imp().options.replace(options);
        view.imp().adjustment.replace(Some(adjustment.clone()));
        let weak = view.downgrade();
        adjustment.connect_value_changed(move |adjustment| {
            if let Some(view) = weak.upgrade() {
                view.follow_adjustment(adjustment.value());
            }
        });
        view
    }

    pub(crate) fn set_look(&self, look: Look) {
        let font_changed = {
            let current = self.imp().look.borrow();
            current.font_family != look.font_family || current.font_points != look.font_points
        };
        if let Some(session) = self.imp().session.borrow_mut().as_mut()
            && let Err(error) = session.set_default_colors(&look.colors)
        {
            tracing::warn!(%error, "terminal colors were not applied");
        }
        self.imp().look.replace(look);
        if font_changed {
            self.reset_font();
        } else {
            self.imp().rows.borrow_mut().clear();
            self.queue_draw();
        }
    }

    /// The working directory for the shell, read when it starts.
    pub(crate) fn set_cwd(&self, cwd: Option<PathBuf>) {
        self.imp().options.borrow_mut().cwd = cwd;
    }

    fn add_controllers(&self) {
        let click = gtk::GestureClick::new();
        let weak = self.downgrade();
        click.connect_pressed(move |_, _, _, _| {
            if let Some(view) = weak.upgrade() {
                view.grab_focus();
            }
        });
        self.add_controller(click);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = self.downgrade();
        scroll.connect_scroll(move |controller, _, dy| {
            weak.upgrade().map_or(glib::Propagation::Proceed, |view| {
                view.wheel(controller, dy)
            })
        });
        self.add_controller(scroll);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(redraw);
        focus.connect_leave(redraw);
        self.add_controller(focus);

        self.connect_scale_factor_notify(Self::reset_font);
    }

    fn font(&self) -> pango::FontDescription {
        if let Some(font) = self.imp().font.borrow().as_ref() {
            return font.clone();
        }
        let look = self.imp().look.borrow();
        let family = look.font_family.as_deref().map_or_else(
            || DEFAULT_FONT.to_owned(),
            |family| format!("{family},{DEFAULT_FONT}"),
        );
        let mut font = pango::FontDescription::from_string(&family);
        #[expect(clippy::cast_possible_truncation, reason = "font sizes are small")]
        font.set_size(
            (look.font_points.unwrap_or(DEFAULT_FONT_POINTS) * f64::from(pango::SCALE)) as i32,
        );
        drop(look);
        self.imp()
            .metrics
            .set(paint::metrics(&self.pango_context(), &font));
        self.imp().font.replace(Some(font.clone()));
        font
    }

    /// Drops the font, its metrics and every cached row, then refits.
    fn reset_font(&self) {
        self.imp().font.replace(None);
        self.imp().rows.borrow_mut().clear();
        self.font();
        self.fit(self.width(), self.height());
        self.queue_draw();
    }

    /// Starts the shell on the first allocation and resizes it on later ones.
    fn fit(&self, width: i32, height: i32) {
        if width <= 0 || height <= 0 || self.imp().exited.get() {
            return;
        }
        self.font();
        let metrics = self.imp().metrics.get();
        let size = grid_size(width, height, metrics);
        let started = self.imp().session.borrow().is_some();
        if !started {
            self.start(size);
            return;
        }
        let resized = self
            .imp()
            .session
            .borrow_mut()
            .as_mut()
            .map(|session| session.resize(size));
        if let Some(Err(error)) = resized {
            tracing::warn!(%error, "terminal resize failed");
        }
        self.sync_adjustment();
    }

    fn start(&self, size: Size) {
        let options = self.imp().options.borrow().clone();
        let mut session = match Session::spawn(&options, size) {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(%error, "the terminal shell did not start");
                self.imp().exited.set(true);
                return;
            }
        };
        if let Err(error) = session.set_default_colors(&self.imp().look.borrow().colors) {
            tracing::warn!(%error, "terminal colors were not applied");
        }
        let fd = match session.poll_fd() {
            Ok(fd) => fd,
            Err(error) => {
                tracing::warn!(%error, "the terminal cannot be watched");
                self.imp().exited.set(true);
                return;
            }
        };
        self.imp().session.replace(Some(session));
        let weak = self.downgrade();
        let watch = gio_unix::InputStream::take_fd(fd)
            .create_source(
                None::<&gio::Cancellable>,
                Some("strata-terminal-read"),
                glib::Priority::DEFAULT,
                move |_| {
                    weak.upgrade()
                        .map_or(glib::ControlFlow::Break, |view| view.on_readable())
                },
            )
            .attach(None);
        self.imp().read_watch.replace(Some(watch));
    }

    fn on_readable(&self) -> glib::ControlFlow {
        let pumped = self.imp().session.borrow_mut().as_mut().map(Session::pump);
        match pumped {
            Some(Ok(pumped)) if !pumped.hung_up => {
                self.after_io();
                glib::ControlFlow::Continue
            }
            Some(Err(error)) => {
                tracing::warn!(%error, "terminal read failed");
                self.finish()
            }
            _ => self.finish(),
        }
    }

    /// The shell is gone: keep its last screen and stop watching.
    fn finish(&self) -> glib::ControlFlow {
        self.imp().read_watch.take();
        if let Some(watch) = self.imp().write_watch.take() {
            watch.remove();
        }
        self.imp().exited.set(true);
        self.queue_draw();
        glib::ControlFlow::Break
    }

    fn after_io(&self) {
        self.watch_writes();
        self.sync_adjustment();
        self.queue_draw();
    }

    /// Keeps a write watch only while queued input waits for the PTY.
    fn watch_writes(&self) {
        let session = self.imp().session.borrow();
        let Some(session) = session.as_ref() else {
            return;
        };
        let waiting = session.wants_write();
        let mut watch = self.imp().write_watch.borrow_mut();
        if !waiting {
            if let Some(watch) = watch.take() {
                watch.remove();
            }
            return;
        }
        if watch.is_none() {
            let fd = match session.poll_fd() {
                Ok(fd) => fd,
                Err(error) => {
                    tracing::warn!(%error, "the terminal cannot be watched for writing");
                    return;
                }
            };
            let weak = self.downgrade();
            *watch = Some(
                gio_unix::OutputStream::take_fd(fd)
                    .create_source(
                        None::<&gio::Cancellable>,
                        Some("strata-terminal-write"),
                        glib::Priority::DEFAULT,
                        move |_| {
                            weak.upgrade()
                                .map_or(glib::ControlFlow::Break, |view| view.on_writable())
                        },
                    )
                    .attach(None),
            );
        }
    }

    fn on_writable(&self) -> glib::ControlFlow {
        let flushed = self.imp().session.borrow_mut().as_mut().map(Session::flush);
        if let Some(Err(error)) = flushed {
            tracing::warn!(%error, "terminal write failed");
        }
        let waiting = self
            .imp()
            .session
            .borrow()
            .as_ref()
            .is_some_and(Session::wants_write);
        if waiting {
            glib::ControlFlow::Continue
        } else {
            self.imp().write_watch.take();
            glib::ControlFlow::Break
        }
    }

    fn stop(&self) {
        for watch in [&self.imp().read_watch, &self.imp().write_watch] {
            if let Some(watch) = watch.take() {
                watch.remove();
            }
        }
        self.imp().session.take();
    }

    fn paint(&self, snapshot: &gtk::Snapshot) {
        let font = self.font();
        let metrics = self.imp().metrics.get();
        let context = self.pango_context();
        let painter = Painter {
            context: &context,
            font: &font,
            metrics,
        };
        let mut session = self.imp().session.borrow_mut();
        let Some(session) = session.as_mut() else {
            return;
        };
        let frame = match session.frame() {
            Ok((frame, _)) => frame,
            Err(error) => {
                tracing::warn!(%error, "terminal frame failed");
                return;
            }
        };
        let mut cache = self.imp().rows.borrow_mut();
        let mut kept = HashMap::with_capacity(frame.rows.len());
        for (y, row) in frame.rows.iter().enumerate() {
            let node = match cache.remove(&row.id) {
                Some((version, node)) if version == row.version => Some(node),
                _ => painter.row(row),
            };
            let Some(node) = node else {
                continue;
            };
            #[expect(
                clippy::cast_precision_loss,
                reason = "a screen has at most a few hundred rows"
            )]
            let top = y as f32 * metrics.cell_height;
            snapshot.save();
            snapshot.translate(&graphene::Point::new(0.0, top));
            snapshot.append_node(&node);
            snapshot.restore();
            kept.insert(row.id, (row.version, node));
        }
        *cache = kept;
        if let Some(cursor) = &frame.cursor
            && !self.imp().exited.get()
        {
            painter.cursor(snapshot, frame, cursor, self.has_focus());
        }
    }

    fn wheel(&self, controller: &gtk::EventControllerScroll, dy: f64) -> glib::Propagation {
        let lines = if controller.unit() == gtk::gdk::ScrollUnit::Surface {
            dy / f64::from(self.imp().metrics.get().cell_height)
        } else {
            dy * WHEEL_LINES
        } + self.imp().scroll_rest.get();
        let whole = lines.trunc();
        self.imp().scroll_rest.set(lines - whole);
        if whole != 0.0 {
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a scroll gesture moves a few lines"
            )]
            self.scroll(ScrollViewport::Delta(whole as isize));
        }
        glib::Propagation::Stop
    }

    fn scroll(&self, scroll: ScrollViewport) {
        let scrolled = self
            .imp()
            .session
            .borrow_mut()
            .as_mut()
            .map(|session| session.scroll(scroll));
        if let Some(Err(error)) = scrolled {
            tracing::warn!(%error, "terminal scroll failed");
        }
        self.sync_adjustment();
        self.queue_draw();
    }

    fn follow_adjustment(&self, value: f64) {
        if self.imp().syncing_adjustment.get() {
            return;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the adjustment holds a non-negative row offset"
        )]
        self.scroll(ScrollViewport::Row(value.max(0.0) as usize));
    }

    fn sync_adjustment(&self) {
        let Some(bar) = self.imp().session.borrow().as_ref().map(Session::scrollbar) else {
            return;
        };
        let Some(adjustment) = self.imp().adjustment.borrow().clone() else {
            return;
        };
        #[expect(clippy::cast_precision_loss, reason = "row counts stay far below 2^52")]
        let (offset, total, visible) = (bar.offset as f64, bar.total as f64, bar.visible as f64);
        self.imp().syncing_adjustment.set(true);
        adjustment.configure(offset, 0.0, total, 1.0, visible, visible);
        self.imp().syncing_adjustment.set(false);
    }
}

#[cfg(test)]
impl TerminalView {
    pub(super) fn screen_text(&self) -> Option<String> {
        let session = self.imp().session.borrow();
        strata_term::plain_text(session.as_ref()?.terminal()).ok()
    }

    pub(super) fn cached_rows(&self) -> usize {
        self.imp().rows.borrow().len()
    }

    pub(super) fn shell_pid(&self) -> Option<u32> {
        self.imp().session.borrow().as_ref().map(Session::pid)
    }
}

fn grid_size(width: i32, height: i32, metrics: Metrics) -> Size {
    let cells = |pixels: i32, cell: f32| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "widget sizes are small positive pixel counts"
        )]
        let count = (pixels as f32 / cell).floor().max(1.0) as u16;
        count
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "cells are a few dozen pixels"
    )]
    Size {
        cols: cells(width, metrics.cell_width),
        rows: cells(height, metrics.cell_height),
        cell_width_px: metrics.cell_width as u32,
        cell_height_px: metrics.cell_height as u32,
    }
}

fn redraw(controller: &impl IsA<gtk::EventController>) {
    if let Some(widget) = controller.widget() {
        widget.queue_draw();
    }
}
