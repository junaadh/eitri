//! Rendering of diagnostics with source excerpts, in the layout of
//! codespan-reporting, drawn with rounded box characters by default:
//!
//! ```text
//! error[E0401]: `if` branches have different types
//!   ╭─ init.eit:2:9
//!   │
//! 2 │   let x = if ready {
//!   │ ╭─────────^
//! 3 │ │     1
//!   │ │     - this is `i64`
//! 4 │ │ } else {
//! 5 │ │     false
//!   │ │     ^^^^^ expected `i64`, found `bool`
//! 6 │ │ }
//!   │ ╰─^ in this `if`
//!   │
//!   = help: make both branches the same type
//! ```
//!
//! [`Chars::ascii`] gives a rustc-like fallback, and [`DisplayStyle`] selects
//! the one-line `Short` and the excerpt-free `Medium` forms.
//!
//! Rendering is the cold path. Line tables are built lazily, once per source
//! per renderer, in time linear in the source; each diagnostic then costs
//! O(labels × log lines + excerpt length). Spans that are out of range or not
//! on character boundaries are clamped rather than trusted, so a producer bug
//! cannot make reporting panic.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::diagnostic::{Diagnostic, LabelStyle, NoteKind, Severity};
use eitri_base::{Idx, SourceId};

use crate::files::{Files, Location};
use crate::width::char_width;

/// How much of a diagnostic to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayStyle {
    /// Header, source excerpts with labels, and notes.
    Rich,
    /// `file:line:col: error[E0301]: message`, then the notes.
    Medium,
    /// `file:line:col: error[E0301]: message` on one line.
    Short,
}

/// The characters excerpts are drawn with.
#[derive(Clone, Copy, Debug)]
pub struct Chars {
    /// Before the location of the first source section.
    pub snippet_start: &'static str,
    /// Before the location of each further source section (another file).
    pub snippet_continue: &'static str,
    pub border: char,
    /// In the border where lines are skipped.
    pub border_break: char,
    pub note_bullet: char,
    pub primary_caret: char,
    pub secondary_caret: char,
    /// Where a multi-line label starts and ends.
    pub multi_primary_caret: char,
    pub multi_secondary_caret: char,
    pub multi_top_left: char,
    pub multi_bottom_left: char,
    pub multi_horizontal: char,
    pub multi_left: char,
    /// Connects a caret to its message on a later row.
    pub pointer: char,
}

impl Chars {
    /// Box drawing with rounded corners. The default.
    pub const fn unicode() -> Self {
        Self {
            snippet_start: "╭─",
            snippet_continue: "├─",
            border: '│',
            border_break: '·',
            note_bullet: '=',
            primary_caret: '^',
            secondary_caret: '-',
            multi_primary_caret: '^',
            multi_secondary_caret: '\'',
            multi_top_left: '╭',
            multi_bottom_left: '╰',
            multi_horizontal: '─',
            multi_left: '│',
            pointer: '│',
        }
    }

    /// ASCII only, for terminals and logs without box drawing.
    pub const fn ascii() -> Self {
        Self {
            snippet_start: "-->",
            snippet_continue: ":::",
            border: '|',
            border_break: '.',
            note_bullet: '=',
            primary_caret: '^',
            secondary_caret: '-',
            multi_primary_caret: '^',
            multi_secondary_caret: '\'',
            multi_top_left: '/',
            multi_bottom_left: '\\',
            multi_horizontal: '-',
            multi_left: '|',
            pointer: '|',
        }
    }
}

impl Default for Chars {
    fn default() -> Self {
        Self::unicode()
    }
}

/// ANSI SGR sequences used when [`RenderOptions::color`] is set.
#[derive(Clone, Copy, Debug)]
pub struct Styles {
    /// Header and primary labels of each severity.
    pub warning: &'static str,
    pub error: &'static str,
    pub fault: &'static str,
    pub bug: &'static str,
    /// The header message.
    pub message: &'static str,
    pub secondary: &'static str,
    /// Line numbers, borders, and note bullets.
    pub border: &'static str,
    /// The `note` and `help` words.
    pub note: &'static str,
}

impl Default for Styles {
    fn default() -> Self {
        Self {
            warning: "\x1b[1;33m",
            error: "\x1b[1;31m",
            fault: "\x1b[1;35m",
            bug: "\x1b[1;31m",
            message: "\x1b[1m",
            secondary: "\x1b[1;34m",
            border: "\x1b[1;34m",
            note: "\x1b[1m",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    pub style: DisplayStyle,
    pub chars: Chars,
    /// Emit ANSI colors. Hosts decide, e.g. only when writing to a terminal.
    pub color: bool,
    pub styles: Styles,
    /// Columns a tab expands to in excerpts.
    pub tab_width: usize,
    /// Lines shown after the first line of a multi-line label before a break.
    pub start_context_lines: usize,
    /// Lines shown before the last line of a multi-line label after a break.
    pub end_context_lines: usize,
    /// Unlabelled lines shown above and below every label.
    pub before_label_lines: usize,
    pub after_label_lines: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            style: DisplayStyle::Rich,
            chars: Chars::unicode(),
            color: false,
            styles: Styles::default(),
            tab_width: 4,
            start_context_lines: 3,
            end_context_lines: 1,
            before_label_lines: 0,
            after_label_lines: 0,
        }
    }
}

/// Renders diagnostics against a [`Files`] registry, caching line tables.
pub struct Renderer<'f, 'a> {
    files: &'f Files<'a>,
    options: RenderOptions,
    line_starts: Vec<Option<Box<[u32]>>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Paint {
    Plain,
    Message,
    Border,
    Note,
    Primary(Severity),
    Secondary,
}

/// A label confined to one line, in display columns of that line.
struct Single<'d> {
    start: usize,
    end: usize,
    style: LabelStyle,
    message: &'d str,
}

/// A label spanning lines, drawn in its own slot left of the source text.
struct Multi<'d> {
    first: usize,
    last: usize,
    /// Display column of the first character, on `first`.
    start: usize,
    /// Display column of the last character, on `last`.
    end: usize,
    /// Only whitespace precedes the start, so the corner sits on the line.
    inline_top: bool,
    style: LabelStyle,
    message: &'d str,
}

/// Everything shown for one source.
struct Section<'d> {
    source: SourceId,
    severity: Severity,
    anchor: Location,
    singles: BTreeMap<usize, Vec<Single<'d>>>,
    multis: Vec<Multi<'d>>,
    lines: BTreeSet<usize>,
}

/// Progress of each multi-line label while one line's rows are drawn.
#[derive(Clone, Copy, Default)]
struct SlotState {
    topped: bool,
    closed: bool,
}

/// A row of display cells, so markers and connectors can be placed by column.
#[derive(Default)]
struct Row {
    cells: Vec<(char, Paint)>,
}

impl Row {
    fn put(&mut self, column: usize, c: char, paint: Paint) {
        if self.cells.len() <= column {
            self.cells.resize(column + 1, (' ', Paint::Plain));
        }
        self.cells[column] = (c, paint);
    }

    /// Like `put`, but never overwrites a drawn cell.
    fn fill(&mut self, column: usize, c: char, paint: Paint) {
        if self.cells.get(column).is_none_or(|&(old, _)| old == ' ') {
            self.put(column, c, paint);
        }
    }

    fn put_str(&mut self, column: usize, text: &str, paint: Paint) {
        for (i, c) in text.chars().enumerate() {
            self.put(column + i, c, paint);
        }
    }

    fn is_blank(&self) -> bool {
        self.cells.iter().all(|&(c, _)| c == ' ')
    }
}

impl<'f, 'a> Renderer<'f, 'a> {
    pub fn new(files: &'f Files<'a>, options: RenderOptions) -> Self {
        Self {
            files,
            options,
            line_starts: Vec::new(),
        }
    }

    /// Resolve a byte offset to a 1-based line and column, or `None` for an
    /// unknown source. The offset is clamped into the source.
    pub fn location(&mut self, source: SourceId, offset: u32) -> Option<Location> {
        let text = self.files.get(source)?.text;
        let offset = clamp(text, offset);
        let starts = self.starts(source);
        let line = line_of(starts, offset);
        let line_start = starts[line] as usize;
        Some(Location {
            line: line as u32 + 1,
            column: text[line_start..offset].chars().count() as u32 + 1,
        })
    }

    pub fn render(&mut self, diagnostic: &Diagnostic) -> String {
        let mut out = String::new();
        self.write(&mut out, diagnostic);
        out
    }

    /// Append the rendering of one diagnostic, ending with a newline.
    pub fn write(&mut self, out: &mut String, diagnostic: &Diagnostic) {
        match self.options.style {
            DisplayStyle::Rich => self.write_rich(out, diagnostic),
            DisplayStyle::Medium | DisplayStyle::Short => {
                let anchor = diagnostic
                    .primary()
                    .or(diagnostic.labels.first())
                    .filter(|label| self.files.get(label.source).is_some());
                if let Some(label) = anchor {
                    let name = self.files.get(label.source).unwrap().name;
                    let at = self.location(label.source, label.span.start).unwrap();
                    write!(out, "{name}:{}:{}: ", at.line, at.column).unwrap();
                }
                self.header(out, diagnostic);
                if self.options.style == DisplayStyle::Medium {
                    self.notes(out, diagnostic, "");
                }
            }
        }
    }

    fn header(&self, out: &mut String, diagnostic: &Diagnostic) {
        let severity = diagnostic.severity;
        let mut head = severity.as_str().to_owned();
        if let Some(code) = diagnostic.code {
            write!(head, "[{code}]").unwrap();
        }
        self.paint(out, Paint::Primary(severity), &head);
        self.paint(out, Paint::Message, &format!(": {}", diagnostic.message));
        out.push('\n');
    }

    fn notes(&self, out: &mut String, diagnostic: &Diagnostic, pad: &str) {
        for note in &diagnostic.notes {
            let kind = match note.kind {
                NoteKind::Note => "note",
                NoteKind::Help => "help",
            };
            out.push_str(pad);
            self.paint(
                out,
                Paint::Border,
                &self.options.chars.note_bullet.to_string(),
            );
            out.push(' ');
            self.paint(out, Paint::Note, kind);
            out.push_str(": ");
            // Continuation lines align with the first line's text.
            let indent = " ".repeat(pad.len() + 2 + kind.len() + 2);
            for (i, line) in note.message.lines().enumerate() {
                if i > 0 {
                    out.push_str(&indent);
                }
                out.push_str(line);
                out.push('\n');
            }
            if note.message.is_empty() {
                out.push('\n');
            }
        }
    }

    fn write_rich(&mut self, out: &mut String, diagnostic: &Diagnostic) {
        self.header(out, diagnostic);

        // One section per source: the primary label's first, then the
        // others in order of first appearance. Unknown sources are skipped.
        let mut order: Vec<SourceId> = Vec::new();
        let primary = diagnostic.primary().map(|label| label.source);
        for source in primary
            .into_iter()
            .chain(diagnostic.labels.iter().map(|label| label.source))
        {
            if self.files.get(source).is_some() && !order.contains(&source) {
                order.push(source);
            }
        }
        let sections: Vec<_> = order
            .into_iter()
            .map(|source| self.section(source, diagnostic))
            .collect();

        let last_line = sections
            .iter()
            .filter_map(|section| section.lines.last())
            .max()
            .map_or(1, |line| line + 1);
        let width = last_line.to_string().len();
        let pad = " ".repeat(width + 1);

        for (index, section) in sections.iter().enumerate() {
            let chars = self.options.chars;
            let name = self.files.get(section.source).unwrap().name;
            out.push_str(&pad);
            let start = if index == 0 {
                chars.snippet_start
            } else {
                chars.snippet_continue
            };
            self.paint(out, Paint::Border, start);
            let at = section.anchor;
            writeln!(out, " {name}:{}:{}", at.line, at.column).unwrap();
            self.annotation_row(out, &pad, &Row::default());

            let mut previous: Option<usize> = None;
            for &line in &section.lines {
                match previous {
                    // A single skipped line costs no more than a break.
                    Some(p) if line == p + 2 => self.source_row(out, section, p + 1, width),
                    Some(p) if line > p + 2 => self.break_row(out, section, p, line, width),
                    _ => {}
                }
                self.source_row(out, section, line, width);
                self.line_annotations(out, section, line, &pad);
                previous = Some(line);
            }
        }

        if !diagnostic.notes.is_empty() {
            let pad = if sections.is_empty() {
                "  ".to_owned()
            } else {
                self.annotation_row(out, &pad, &Row::default());
                pad
            };
            self.notes(out, diagnostic, &pad);
        }
    }

    fn starts(&mut self, source: SourceId) -> &[u32] {
        let index = source.index();
        if self.line_starts.len() <= index {
            self.line_starts.resize_with(index + 1, || None);
        }
        let text = self.files.get(source).map_or("", |file| file.text);
        self.line_starts[index].get_or_insert_with(|| {
            let newlines = text.bytes().enumerate().filter(|&(_, b)| b == b'\n');
            std::iter::once(0)
                .chain(newlines.map(|(i, _)| i as u32 + 1))
                .collect()
        })
    }

    /// Number of displayable lines; a final newline does not start a line.
    fn line_count(&mut self, source: SourceId) -> usize {
        let text = self.files.get(source).unwrap().text;
        let lines = self.starts(source).len();
        if text.ends_with('\n') {
            lines - 1
        } else {
            lines
        }
        .max(1)
    }

    /// The text of a 0-based line, without its line terminator.
    fn line_text(&mut self, source: SourceId, line: usize) -> (usize, &'a str) {
        let text = self.files.get(source).unwrap().text;
        let starts = self.starts(source);
        let Some(&start) = starts.get(line) else {
            return (text.len(), "");
        };
        let start = start as usize;
        let end = starts
            .get(line + 1)
            .map_or(text.len(), |&next| next as usize - 1);
        let line_text = &text[start..end];
        (start, line_text.strip_suffix('\r').unwrap_or(line_text))
    }

    /// Display columns of `text[..len]`.
    fn columns(&self, text: &str, len: usize) -> usize {
        text[..len.min(text.len())]
            .chars()
            .map(|c| char_width(c, self.options.tab_width))
            .sum()
    }

    fn section<'d>(&mut self, source: SourceId, diagnostic: &'d Diagnostic) -> Section<'d> {
        let text = self.files.get(source).unwrap().text;
        let line_count = self.line_count(source);
        let options = self.options;
        let mut section = Section {
            source,
            severity: diagnostic.severity,
            anchor: Location { line: 1, column: 1 },
            singles: BTreeMap::new(),
            multis: Vec::new(),
            lines: BTreeSet::new(),
        };

        let labels = diagnostic
            .labels
            .iter()
            .filter(|label| label.source == source);
        let anchor = labels
            .clone()
            .min_by_key(|label| (label.style != LabelStyle::Primary, label.span.start))
            .unwrap();
        section.anchor = self.location(source, anchor.span.start).unwrap();

        for label in labels {
            let start = clamp(text, label.span.start);
            let end = clamp(text, label.span.end).max(start);
            let starts = self.starts(source);
            let first = line_of(starts, start);
            // An exclusive end at a line start belongs to the previous line.
            let last = if end > start {
                line_of(starts, end - 1)
            } else {
                first
            };

            let (first_start, first_text) = self.line_text(source, first);
            let start_column = self.columns(first_text, start - first_start);
            if first == last {
                let end_column = self.columns(first_text, end - first_start);
                section.singles.entry(first).or_default().push(Single {
                    start: start_column,
                    end: end_column.max(start_column + 1),
                    style: label.style,
                    message: &label.message,
                });
            } else {
                let (last_start, last_text) = self.line_text(source, last);
                let end_column = self.columns(last_text, end - last_start);
                section.multis.push(Multi {
                    first,
                    last,
                    start: start_column,
                    end: end_column.saturating_sub(1),
                    inline_top: first_text[..start - first_start].trim().is_empty(),
                    style: label.style,
                    message: &label.message,
                });
                let head = (first + 1)..=(first + options.start_context_lines).min(last);
                let tail = last.saturating_sub(options.end_context_lines).max(first)..last;
                section.lines.extend(head.chain(tail));
            }
            let above = first.saturating_sub(options.before_label_lines)..first;
            let below = (last + 1)..(last + 1 + options.after_label_lines).min(line_count);
            section
                .lines
                .extend(above.chain([first, last]).chain(below));
        }
        // Outer labels take the leftmost slots so their bars enclose inner ones.
        section
            .multis
            .sort_by_key(|multi| (multi.first, usize::MAX - multi.last, multi.start));
        section
    }

    fn source_row(&mut self, out: &mut String, section: &Section, line: usize, width: usize) {
        let chars = self.options.chars;
        let (_, text) = self.line_text(section.source, line);
        self.paint(
            out,
            Paint::Border,
            &format!("{:>width$} {}", line + 1, chars.border),
        );

        let mut slots = Row::default();
        for (slot, multi) in section.multis.iter().enumerate() {
            let c = if line == multi.first && multi.inline_top {
                chars.multi_top_left
            } else if multi.first < line && line <= multi.last {
                chars.multi_left
            } else {
                continue;
            };
            slots.put(2 * slot, c, label_paint(multi.style, section.severity));
        }
        if !text.is_empty() {
            slots.put(2 * section.multis.len(), ' ', Paint::Plain);
        }
        if !slots.is_blank() || !text.is_empty() {
            out.push(' ');
            let len = if text.is_empty() {
                slots.cells.len()
            } else {
                2 * section.multis.len()
            };
            self.cells(out, &slots.cells[..len]);
            for c in text.chars() {
                match c {
                    '\t' => out.extend(std::iter::repeat_n(' ', self.options.tab_width)),
                    c if c.is_control() => out.push('\u{FFFD}'),
                    c => out.push(c),
                }
            }
        }
        out.push('\n');
    }

    fn break_row(
        &self,
        out: &mut String,
        section: &Section,
        before: usize,
        after: usize,
        width: usize,
    ) {
        let mut row = Row::default();
        for (slot, multi) in section.multis.iter().enumerate() {
            if multi.first <= before && after <= multi.last {
                row.put(
                    2 * slot,
                    self.options.chars.multi_left,
                    label_paint(multi.style, section.severity),
                );
            }
        }
        out.push_str(&" ".repeat(width + 1));
        self.paint(
            out,
            Paint::Border,
            &self.options.chars.border_break.to_string(),
        );
        self.trailing(out, &row);
    }

    /// The rows under one source line: multi-line tops, single-line
    /// underlines with their messages, then multi-line bottoms.
    fn line_annotations(&self, out: &mut String, section: &Section, line: usize, pad: &str) {
        let chars = self.options.chars;
        let offset = 2 * section.multis.len();
        let mut state = vec![SlotState::default(); section.multis.len()];
        let bars = |state: &[SlotState]| {
            let mut row = Row::default();
            for (slot, multi) in section.multis.iter().enumerate() {
                let started = multi.first < line || multi.inline_top || state[slot].topped;
                if multi.first <= line && line <= multi.last && started && !state[slot].closed {
                    row.put(
                        2 * slot,
                        chars.multi_left,
                        label_paint(multi.style, section.severity),
                    );
                }
            }
            row
        };

        for (slot, multi) in section.multis.iter().enumerate() {
            if multi.first != line || multi.inline_top {
                continue;
            }
            let paint = label_paint(multi.style, section.severity);
            let mut row = bars(&state);
            row.put(2 * slot, chars.multi_top_left, paint);
            for column in 2 * slot + 1..offset + multi.start {
                row.fill(column, chars.multi_horizontal, paint);
            }
            let caret = match multi.style {
                LabelStyle::Primary => chars.multi_primary_caret,
                LabelStyle::Secondary => chars.multi_secondary_caret,
            };
            row.put(offset + multi.start, caret, paint);
            self.annotation_row(out, pad, &row);
            state[slot].topped = true;
        }

        if let Some(singles) = section.singles.get(&line) {
            for mut row in self.single_rows(singles, offset, section.severity) {
                for (column, cell) in bars(&state).cells.into_iter().enumerate() {
                    if cell.0 != ' ' {
                        row.put(column, cell.0, cell.1);
                    }
                }
                self.annotation_row(out, pad, &row);
            }
        }

        for (slot, multi) in section.multis.iter().enumerate() {
            if multi.last != line {
                continue;
            }
            let paint = label_paint(multi.style, section.severity);
            let mut row = bars(&state);
            row.put(2 * slot, chars.multi_bottom_left, paint);
            for column in 2 * slot + 1..offset + multi.end {
                row.fill(column, chars.multi_horizontal, paint);
            }
            let caret = match multi.style {
                LabelStyle::Primary => chars.multi_primary_caret,
                LabelStyle::Secondary => chars.multi_secondary_caret,
            };
            row.put(offset + multi.end, caret, paint);
            if !multi.message.is_empty() {
                row.put_str(offset + multi.end + 2, multi.message, paint);
            }
            self.annotation_row(out, pad, &row);
            state[slot].closed = true;
        }
    }

    /// Underline row plus stacked message rows for one line's single labels,
    /// shifted right by `offset` columns of multi-line slots.
    fn single_rows(&self, singles: &[Single], offset: usize, severity: Severity) -> Vec<Row> {
        let chars = self.options.chars;
        let paint = |style| label_paint(style, severity);
        let mut underline = Row::default();
        // Secondary first, so a primary underline wins where they overlap.
        for style in [LabelStyle::Secondary, LabelStyle::Primary] {
            let c = match style {
                LabelStyle::Primary => chars.primary_caret,
                LabelStyle::Secondary => chars.secondary_caret,
            };
            for single in singles.iter().filter(|single| single.style == style) {
                for column in single.start..single.end {
                    underline.put(offset + column, c, paint(style));
                }
            }
        }

        let mut labeled: Vec<&Single> = singles.iter().filter(|s| !s.message.is_empty()).collect();
        labeled.sort_by_key(|single| single.start);
        let max_end = singles.iter().map(|single| single.end).max().unwrap_or(0);
        // One message reaching the end of the underline goes inline after it;
        // the others hang below on pointers, rightmost first.
        let inline = (0..labeled.len())
            .filter(|&i| labeled[i].end == max_end)
            .max_by_key(|&i| labeled[i].start);
        if let Some(i) = inline {
            let single = labeled.remove(i);
            underline.put_str(offset + max_end + 1, single.message, paint(single.style));
        }
        let mut rows = vec![underline];
        for i in (0..labeled.len()).rev() {
            let mut pointers = Row::default();
            for single in &labeled[..=i] {
                pointers.put(offset + single.start, chars.pointer, paint(single.style));
            }
            let mut message = Row::default();
            for single in &labeled[..i] {
                message.put(offset + single.start, chars.pointer, paint(single.style));
            }
            let single = labeled[i];
            message.put_str(offset + single.start, single.message, paint(single.style));
            rows.push(pointers);
            rows.push(message);
        }
        rows
    }

    fn annotation_row(&self, out: &mut String, pad: &str, row: &Row) {
        out.push_str(pad);
        self.paint(out, Paint::Border, &self.options.chars.border.to_string());
        self.trailing(out, row);
    }

    /// Write a row after a border, without trailing blanks, then a newline.
    fn trailing(&self, out: &mut String, row: &Row) {
        let len = row
            .cells
            .iter()
            .rposition(|&(c, _)| c != ' ')
            .map_or(0, |i| i + 1);
        if len > 0 {
            out.push(' ');
            self.cells(out, &row.cells[..len]);
        }
        out.push('\n');
    }

    fn cells(&self, out: &mut String, mut cells: &[(char, Paint)]) {
        while let Some(&(_, paint)) = cells.first() {
            let run = cells.iter().take_while(|&&(_, p)| p == paint).count();
            let text: String = cells[..run].iter().map(|&(c, _)| c).collect();
            self.paint(out, paint, &text);
            cells = &cells[run..];
        }
    }

    fn paint(&self, out: &mut String, paint: Paint, text: &str) {
        let styles = &self.options.styles;
        let escape = match paint {
            Paint::Plain => "",
            Paint::Message => styles.message,
            Paint::Border => styles.border,
            Paint::Note => styles.note,
            Paint::Secondary => styles.secondary,
            Paint::Primary(Severity::Warning) => styles.warning,
            Paint::Primary(Severity::Error) => styles.error,
            Paint::Primary(Severity::Fault) => styles.fault,
            Paint::Primary(Severity::Bug) => styles.bug,
        };
        if self.options.color && !escape.is_empty() {
            out.push_str(escape);
            out.push_str(text);
            out.push_str("\x1b[0m");
        } else {
            out.push_str(text);
        }
    }
}

fn label_paint(style: LabelStyle, severity: Severity) -> Paint {
    match style {
        LabelStyle::Primary => Paint::Primary(severity),
        LabelStyle::Secondary => Paint::Secondary,
    }
}

/// Clamp an offset into `text` and down to a character boundary.
fn clamp(text: &str, offset: u32) -> usize {
    let mut offset = (offset as usize).min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// 0-based index of the line containing `offset`.
fn line_of(starts: &[u32], offset: usize) -> usize {
    starts.partition_point(|&start| start as usize <= offset) - 1
}
