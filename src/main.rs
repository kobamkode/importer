//! The gpui shell — variant B layout as approved in the prototype
//! (bd importer-cb5.1 verdict). All import behavior lives in the `importer`
//! core; this file only wires it up — import view plus the settings form.

use std::ops::Range;
use std::path::PathBuf;

use gpui::{
    actions, AnyElement, App, Application, Bounds, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, Focusable, GlobalElementId,
    KeyBinding, LayoutId, MouseButton, MouseDownEvent, PaintQuad, Pixels, Render, ShapedLine,
    SharedString, Style, Task, TextRun, TitlebarOptions, UTF16Selection, Window, WindowBounds,
    WindowOptions, div, fill, hsla, prelude::*, px, point, relative, rgb, rgba, size,
};
use importer::{
    Outcome, Settings, detect_roots, format_date, import_file, load_settings, save_settings,
    scan, settings_from_form,
};

actions!(settings_field, [Backspace, Delete, SelectAll, Paste, Commit]);

#[derive(Clone, Copy, PartialEq)]
enum RowState {
    Pending,
    Imported,
    Failed,
    Skipped,
}

/// Which page the window shows: the import view or the settings form.
#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Import,
    Settings,
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
    screen: Screen,
    dest_field: Entity<Field>,
    format_field: Entity<Field>,
    save_error: Option<String>,
}

impl Main {
    fn new(cx: &mut Context<Self>) -> Self {
        let settings = load_settings();
        let rows = Self::scan_rows();
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
            screen: Screen::Import,
            dest_field: cx.new(|cx| Field::new("Destination root", cx)),
            format_field: cx.new(|cx| Field::new("YYYYMMDD", cx)),
            save_error: None,
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

    /// A fresh scan as pending rows — Launch and Refresh share this.
    fn scan_rows() -> Vec<Row> {
        scan(&detect_roots())
            .into_iter()
            .map(|src| Row {
                src,
                dest: None,
                state: RowState::Pending,
            })
            .collect()
    }

    /// Launch/Refresh: re-detect mounts, re-scan, reset the run. Sync on
    /// purpose — shortcut: a mount with a huge tree will jank the UI thread;
    /// move the scan to the background executor if that ever bites.
    fn rescan(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        self.rows = Self::scan_rows();
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
                // The Source can vanish mid-run (unmounted/unplugged).
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
                let outcome = cx
                    .background_executor()
                    .spawn(async move { import_file(&src, &settings) })
                    .await;
                let applied = this.update(cx, |this, cx| {
                    this.apply(i, outcome);
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

    /// Gear: swap to the settings form, fields loaded from the current
    /// (persisted) Settings.
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dest = self.settings.dest_root.to_string_lossy().into_owned();
        let fmt = self.settings.date_format.clone();
        self.dest_field.update(cx, |field, cx| field.set_text(&dest, cx));
        self.format_field.update(cx, |field, cx| field.set_text(&fmt, cx));
        self.save_error = None;
        self.screen = Screen::Settings;
        cx.focus_view(&self.dest_field, window);
        cx.notify();
    }

    /// Save button / Enter inside a field: persist, then swap back. The
    /// in-memory Settings is replaced too, so the next import uses the new
    /// values without an app restart (`start` reads `self.settings`).
    fn save_settings_view(&mut self, cx: &mut Context<Self>) {
        let dest = self.dest_field.read(cx).text();
        let fmt = self.format_field.read(cx).text();
        let new = settings_from_form(&dest, &fmt, &self.settings);
        match save_settings(&new) {
            Ok(()) => {
                self.settings = new;
                self.save_error = None;
                self.screen = Screen::Import;
            }
            Err(err) => self.save_error = Some(format!("could not save settings: {err}")),
        }
        cx.notify();
    }

    fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.save_error = None;
        self.screen = Screen::Import;
        cx.notify();
    }

    /// One file's outcome drives its row directly — no re-derivation from
    /// Report counts. Skipped rows carry the Destination that already holds
    /// the content (US13: verify where every file went).
    fn apply(&mut self, i: usize, outcome: Outcome) {
        let Some(row) = self.rows.get_mut(i) else { return };
        match outcome {
            Outcome::Imported(dest) => {
                row.state = RowState::Imported;
                row.dest = Some(dest);
                self.imported += 1;
            }
            Outcome::Skipped(dest) => {
                row.state = RowState::Skipped;
                row.dest = Some(dest);
                self.skipped += 1;
            }
            Outcome::Failed => {
                row.state = RowState::Failed;
                self.failed += 1;
            }
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
                        format!(
                            "{}  →  {} (already in Destination)",
                            row.src.display(),
                            row.dest.as_ref().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default()
                        ),
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

    /// The settings form: two editable fields populated from the persisted
    /// Settings, a live preview from the core's own formatter, Save/Back.
    /// Enter inside a field bubbles up here as Commit and saves.
    fn settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let dest = self.dest_field.read(cx).text();
        let fmt = self.format_field.read(cx).text();
        let sample = chrono::NaiveDate::from_ymd_opt(2026, 10, 10)
            .unwrap()
            .and_hms_opt(14, 32, 55)
            .unwrap();
        let stamped = format_date(sample, &fmt);
        let preview = format!("{dest}/{stamped}/{stamped}_IMG_1234.jpg");
        div().flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .on_action(cx.listener(|this, _: &Commit, _, cx| this.save_settings_view(cx)))
            .child(
                div().w(px(520.))
                    .flex()
                    .flex_col()
                    .gap_5()
                    .pt(px(48.))
                    .child(div().text_xl().child("Settings"))
                    .child(Self::form_field("Destination", self.dest_field.clone()))
                    .child(Self::form_field("Date format", self.format_field.clone()))
                    .child(
                        div().flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div().text_sm()
                                    .text_color(rgb(0x7d8aa0))
                                    .child("Next import will write"),
                            )
                            .child(
                                div().px_4()
                                    .py_3()
                                    .rounded_md()
                                    .bg(rgb(0x1a2029))
                                    .font_family("monospace")
                                    .text_size(px(12.))
                                    .text_color(rgb(0x33dd88))
                                    .child(preview),
                            ),
                    )
                    .child(
                        div().text_xs()
                            .text_color(rgb(0x556677))
                            .child("Tokens: YYYY year · MM month · DD day · HH:MM:SS time — everything else is literal"),
                    )
                    .child(match &self.save_error {
                        Some(err) => div().text_sm().text_color(rgb(0xff6b6b)).child(err.clone()),
                        None => div(),
                    })
                    .child(
                        div().flex()
                            .gap_3()
                            .child(Self::button(cx, "save", "Save", 0x2f6feb, 0x4f8cff, |t, cx| {
                                t.save_settings_view(cx)
                            }))
                            .child(Self::button(cx, "back", "Back", 0x2a3242, 0x3a465e, |t, cx| {
                                t.close_settings(cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn form_field(label: &'static str, field: Entity<Field>) -> impl IntoElement {
        div().flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().text_color(rgb(0x7d8aa0)).child(label))
            .child(field)
    }
}

impl Render for Main {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.screen == Screen::Settings {
            return self.settings_view(cx);
        }
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
                    // Running (including cancelling…): Cancel alone; hiding
                    // Import/Refresh matches what's actually actionable.
                    .child(if self.running {
                        Self::button(cx, "cancel", "Cancel", 0x2a3242, 0x3a465e, |t, cx| {
                            t.cancel(cx)
                        })
                        .into_any_element()
                    } else {
                        Self::button(cx, "import", "Import", 0x2f6feb, 0x4f8cff, |t, cx| {
                            t.start(cx)
                        })
                        .into_any_element()
                    })
                    .child(if self.running {
                        div().into_any_element()
                    } else {
                        Self::button(cx, "refresh", "Refresh", 0x2a3242, 0x3a465e, |t, cx| {
                            t.rescan(cx)
                        })
                        .into_any_element()
                    })
                    // Gear, pushed to the right; opens the settings form.
                    // Hidden while an import runs: settings take effect on
                    // the next import (cb5.6), so a mid-run save would split
                    // one run across two destinations.
                    .child(div().flex_1())
                    .child(if self.running {
                        div().into_any_element()
                    } else {
                        div().id("settings")
                            .px_5()
                            .py_2()
                            .rounded_full()
                            .cursor_pointer()
                            .bg(rgb(0x2a3242))
                            .text_color(rgb(0xffffff))
                            .text_size(px(13.))
                            .hover(move |s| s.bg(rgb(0x3a465e)))
                            .child("⚙ Settings")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings(window, cx)
                            }))
                            .into_any_element()
                    }),
            )
            .into_any_element()
    }
}

// ---------- settings text field ----------
// Minimal single-line text input, stripped down from gpui's own
// examples/input.rs (gpui, MIT/Apache-2.0): focus, typing, backspace, delete,
// select-all, paste; the settings form catches Enter as Commit.
// shortcut: no arrow-key cursor movement or mouse selection — the cursor only
// sits at the typing end (backspace walks it back, cmd/ctrl-a replaces all);
// backspace/delete move one char, not one grapheme (no unicode-segmentation
// dep). Add any of these when mid-string edits actually bite.

struct Field {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: &'static str,
    selected_range: Range<usize>,
}

impl Field {
    fn new(placeholder: &'static str, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder,
            selected_range: 0..0,
        }
    }

    fn text(&self) -> String {
        self.content.to_string()
    }

    fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.content = text.to_string().into();
        self.selected_range = self.content.len()..self.content.len();
        cx.notify();
    }

    fn cursor_offset(&self) -> usize {
        self.selected_range.end
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .map(|(idx, _)| idx)
            .rev()
            .find(|idx| *idx < offset)
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .char_indices()
            .map(|(idx, _)| idx)
            .find(|idx| *idx > offset)
            .unwrap_or(self.content.len())
    }

    // The platform speaks UTF-16 offsets; content is indexed in bytes.
    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range.start)..self.offset_from_utf16(range.end)
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let start = self.previous_boundary(self.cursor_offset());
            self.selected_range = start..self.cursor_offset();
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let end = self.next_boundary(self.cursor_offset());
            self.selected_range = self.cursor_offset()..end;
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn select_all(&mut self, _: &SelectAll, _window: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text, window, cx)
        }
    }

    fn on_mouse_down(&mut self, _: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window);
        cx.notify();
    }
}

impl EntityInputHandler for Field {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: false,
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .unwrap_or_else(|| self.selected_range.clone());
        // Single-line: a newline would corrupt the flat key=value settings file.
        let new_text = new_text.replace(['\n', '\r'], "");
        self.content = (self.content[0..range.start].to_owned()
            + &new_text
            + &self.content[range.end..])
            .into();
        let cursor = range.start + new_text.len();
        self.selected_range = cursor..cursor;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // No IME marked text: treat as a plain replacement.
        self.replace_text_in_range(range_utf16, new_text, window, cx)
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        None // no IME candidate window
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None // no click-to-position
    }
}

impl Focusable for Field {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Field {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex()
            .key_context("settings_field")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .w_full()
            .px_4()
            .py_3()
            .rounded_md()
            .bg(rgb(0x232b3a))
            .border_1()
            .border_color(rgb(0x333d4f))
            .text_size(px(13.))
            .child(FieldText { field: cx.entity() })
    }
}

/// Paints the field's text, placeholder and cursor, and registers the field
/// as the window's input handler for its bounds. Companion element to
/// [`Field`] (same provenance: gpui examples/input.rs).
struct FieldText {
    field: Entity<Field>,
}

struct FieldPrepaint {
    line: Option<ShapedLine>,
    cursor: Option<PaintQuad>,
    selection: Option<PaintQuad>,
}

impl IntoElement for FieldText {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for FieldText {
    type RequestLayoutState = ();
    type PrepaintState = FieldPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let field = self.field.read(cx);
        let content = field.content.clone();
        let cursor = field.cursor_offset();
        let style = window.text_style();
        let (display_text, text_color) = if content.is_empty() {
            (SharedString::from(field.placeholder), hsla(0., 0., 1., 0.25))
        } else {
            (content, style.color)
        };
        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(display_text, font_size, &[run], None);
        let cursor_pos = line.x_for_index(cursor);
        let (selection, cursor) = if field.selected_range.is_empty() {
            (
                None,
                Some(fill(
                    Bounds::new(
                        point(bounds.left() + cursor_pos, bounds.top()),
                        size(px(2.), bounds.bottom() - bounds.top()),
                    ),
                    rgb(0x4f8cff),
                )),
            )
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(
                            bounds.left() + line.x_for_index(field.selected_range.start),
                            bounds.top(),
                        ),
                        point(
                            bounds.left() + line.x_for_index(field.selected_range.end),
                            bounds.bottom(),
                        ),
                    ),
                    rgba(0x4f8cff50),
                )),
                None,
            )
        };
        FieldPrepaint {
            line: Some(line),
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.field.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );
        if let Some(selection) = prepaint.selection.take() {
            window.paint_quad(selection)
        }
        let line = prepaint.line.take().unwrap();
        line.paint(bounds.origin, window.line_height(), window, cx)
            .ok();
        if focus_handle.is_focused(window) {
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
        }
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, None),
            KeyBinding::new("delete", Delete, None),
            KeyBinding::new("cmd-a", SelectAll, None),
            KeyBinding::new("ctrl-a", SelectAll, None),
            KeyBinding::new("ctrl-v", Paste, None),
            KeyBinding::new("enter", Commit, None),
        ]);
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
