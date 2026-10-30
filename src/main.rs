//! The gpui shell — variant B layout as approved in the prototype
//! (bd importer-cb5.1 verdict). All import behavior lives in the `importer`
//! core; this file only wires it up. Settings view arrives with importer-cb5.6.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, Application, Bounds, Context, Render, Task, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, rgb, relative,
};
use importer::{Report, Settings, detect_roots, import_files, load_settings, scan};

#[derive(Clone, Copy, PartialEq)]
enum RowState {
    Pending,
    Imported,
    Failed,
    Skipped,
}

struct Row {
    src: PathBuf,
    dest: Option<PathBuf>,
    state: RowState,
}

struct Main {
    settings: Settings,
    rows: Vec<Row>,
    imported: usize,
    failed: usize,
    skipped: usize,
    running: bool,
    cancelled: bool,
    finished: bool,
    source_lost: bool,
    task: Option<Task<()>>,
}

impl Main {
    fn new(_cx: &mut Context<Self>) -> Self {
        let settings = load_settings();
        let rows = scan(&detect_roots())
            .into_iter()
            .map(|src| Row {
                src,
                dest: None,
                state: RowState::Pending,
            })
            .collect();
        Self {
            settings,
            rows,
            imported: 0,
            failed: 0,
            skipped: 0,
            running: false,
            cancelled: false,
            finished: false,
            source_lost: false,
            task: None,
        }
    }

    fn status(&self) -> &'static str {
        if self.source_lost && !self.running {
            return "source lost — import stopped";
        }
        match (self.running, self.cancelled, self.finished) {
            (true, false, _) => "importing…",
            (true, true, _) => "cancelling…",
            (false, true, _) => "cancelled",
            (false, false, true) => "done",
            (false, false, false) => "ready",
        }
    }

    fn summary(&self) -> Option<String> {
        (!self.running && self.finished).then(|| {
            format!(
                "{} imported · {} failed · {} skipped",
                self.imported, self.failed, self.skipped
            )
        })
    }

    /// Every row the run actually handled — skipped and failed rows consumed
    /// their slot too, so a re-run of already-imported media still reaches
    /// the total instead of parking at `imported/total`.
    fn processed(&self) -> usize {
        self.imported + self.failed + self.skipped
    }

    /// Launch/Refresh: re-detect mounts, re-scan, reset the run. Sync on
    /// purpose — shortcut: a mount with a huge tree will jank the UI thread;
    /// move the scan to the background executor if that ever bites.
    fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        self.rows = scan(&detect_roots())
            .into_iter()
            .map(|src| Row {
                src,
                dest: None,
                state: RowState::Pending,
            })
            .collect();
        self.reset_run();
        cx.notify();
    }

    fn reset_run(&mut self) {
        self.imported = 0;
        self.failed = 0;
        self.skipped = 0;
        self.cancelled = false;
        self.finished = false;
        self.source_lost = false;
    }

    /// Import starts only on click, one file at a time on the background
    /// executor so the window keeps painting; each completion repaints its row.
    fn start(&mut self, cx: &mut Context<Self>) {
        if self.running || self.rows.is_empty() {
            return;
        }
        for row in &mut self.rows {
            row.state = RowState::Pending;
            row.dest = None;
        }
        self.reset_run();
        self.running = true;
        let total = self.rows.len();
        self.task = Some(cx.spawn(async move |this, cx| {
            for i in 0..total {
                let next = this.update(cx, |this, _| {
                    if this.cancelled {
                        return None;
                    }
                    this.rows
                        .get(i)
                        .map(|row| (row.src.clone(), this.settings.clone()))
                });
                let Ok(next) = next else { return };
                let Some((src, settings)) = next else { break };
                // The Source can vanish mid-run (card unmounted/unplugged).
                // Stop instead of walking the rest of the rows as failures —
                // remaining rows stay pending and Refresh recovers.
                if !src.exists() {
                    this.update(cx, |this, cx| {
                        this.source_lost = true;
                        this.running = false;
                        this.finished = true;
                        cx.notify();
                    })
                    .ok();
                    return;
                }
                let report = cx
                    .background_executor()
                    .spawn(async move { import_files(&[src], &settings) })
                    .await;
                let applied = this.update(cx, |this, cx| {
                    this.apply(i, report);
                    cx.notify();
                });
                if applied.is_err() {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.running = false;
                this.finished = true;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Flag-based cancel: the current file finishes (never a partial copy at
    /// the Destination), the loop stops before the next one.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if !self.running || self.cancelled {
            return;
        }
        self.cancelled = true;
        cx.notify();
    }

    fn apply(&mut self, i: usize, report: Report) {
        let Some(row) = self.rows.get_mut(i) else { return };
        if report.imported == 1 {
            row.state = RowState::Imported;
            row.dest = report.mappings.first().map(|(_, dest)| dest.clone());
            self.imported += 1;
        } else if report.failed == 1 {
            row.state = RowState::Failed;
            self.failed += 1;
        } else if report.skipped == 1 {
            row.state = RowState::Skipped;
            self.skipped += 1;
        }
    }

    fn button(
        cx: &mut Context<Self>,
        id: &'static str,
        label: &'static str,
        bg: u32,
        hover_bg: u32,
        f: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        div().id(id)
            .px_5()
            .py_2()
            .rounded_full()
            .cursor_pointer()
            .bg(rgb(bg))
            .text_color(rgb(0xffffff))
            .text_size(px(13.))
            .hover(move |s| s.bg(rgb(hover_bg)))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| f(this, cx)))
    }

    fn header(&self) -> impl IntoElement {
        let total = self.rows.len();
        let frac = if total == 0 {
            0.0
        } else {
            self.processed() as f32 / total as f32
        };
        div().flex()
            .flex_col()
            .gap_3()
            .px_6()
            .py_5()
            .child(
                div().flex()
                    .items_baseline()
                    .justify_between()
                    .child(
                        div().flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                div().text_2xl()
                                    .child(format!("{}", self.processed())),
                            )
                            .child(
                                div().text_xl()
                                    .text_color(rgb(0x7d8aa0))
                                    .child(format!("/ {total}")),
                            ),
                    )
                    .child(
                        div().flex()
                            .items_baseline()
                            .gap_4()
                            .child(
                                div().text_sm()
                                    .text_color(rgb(0x7d8aa0))
                                    .child(self.status()),
                            )
                            .child(match self.summary() {
                                Some(summary) => div()
                                    .text_sm()
                                    .text_color(rgb(0x33dd88))
                                    .child(summary),
                                None => div(),
                            }),
                    ),
            )
            .child(
                div().w_full()
                    .h(px(8.))
                    .rounded_full()
                    .bg(rgb(0x232b3a))
                    .child(
                        div().h_full()
                            .rounded_full()
                            .bg(rgb(0x4f8cff))
                            .w(relative(frac.max(0.004))),
                    ),
            )
    }

    fn rows_area(&self) -> AnyElement {
        if self.rows.is_empty() {
            return div().flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div().text_color(rgb(0x7d8aa0))
                        .child("No media found — connect a card, then press Refresh"),
                )
                .into_any_element();
        }
        div().id("rows")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .children(self.rows.iter().map(|row| {
                let (marker, marker_color, text, text_color) = match row.state {
                    RowState::Pending => (
                        "·",
                        0x44505e,
                        row.src.to_string_lossy().into_owned(),
                        0x77828f,
                    ),
                    RowState::Imported => (
                        "✓",
                        0x33dd88,
                        format!(
                            "{}  →  {}",
                            row.src.display(),
                            row.dest.as_ref().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
                        ),
                        0xd7e0ea,
                    ),
                    RowState::Failed => (
                        "✗",
                        0xff6b6b,
                        format!("{}  (failed)", row.src.display()),
                        0xff6b6b,
                    ),
                    RowState::Skipped => (
                        "=",
                        0x7d8aa0,
                        format!("{}  (already in Destination)", row.src.display()),
                        0x9fb0c3,
                    ),
                };
                div().flex()
                    .gap_3()
                    .px_6()
                    .py(px(6.))
                    .child(div().w(px(14.)).text_color(rgb(marker_color)).child(marker))
                    .child(div().flex_1().truncate().text_color(rgb(text_color)).child(text))
            }))
            .into_any_element()
    }
}

impl Render for Main {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.header();
        let rows_area = self.rows_area();
        div().size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x141a24))
            .text_color(rgb(0xe6edf3))
            .child(header)
            .child(rows_area)
            .child(
                div().flex()
                    .gap_3()
                    .items_center()
                    .px_6()
                    .py_4()
                    .child(Self::button(cx, "import", "Import", 0x2f6feb, 0x4f8cff, |t, cx| {
                        t.start(cx)
                    }))
                    .child(Self::button(cx, "cancel", "Cancel", 0x2a3242, 0x3a465e, |t, cx| {
                        t.cancel(cx)
                    }))
                    .child(Self::button(cx, "refresh", "Refresh", 0x2a3242, 0x3a465e, |t, cx| {
                        t.rescan(cx)
                    })),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, gpui::size(px(960.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("importer".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(Main::new),
        )
        .unwrap();
        cx.activate(true);
    });
}
