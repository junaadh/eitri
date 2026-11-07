//! The diagnostic data model handed to hosts and renderers.

use std::fmt;

use eitri_base::{SourceId, Span};

use crate::codes::Code;

/// How serious a report is. A fault is a runtime failure with a trace;
/// warnings and errors come from checking, before anything runs; a bug is a
/// broken internal invariant (an internal compiler or VM error), reported
/// instead of panicking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Warning,
    Error,
    Fault,
    Bug,
}

impl Severity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Warning => "warning",
            Severity::Error => "error",
            Severity::Fault => "fault",
            Severity::Bug => "bug",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LabelStyle {
    /// The location the diagnostic is about; underlined with `^`.
    Primary,
    /// Related context; underlined with `-`.
    Secondary,
}

/// A source range with an optional message, drawn under the excerpt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub style: LabelStyle,
    pub source: SourceId,
    pub span: Span,
    pub message: String,
}

impl Label {
    pub fn primary(source: SourceId, span: Span) -> Self {
        Self {
            style: LabelStyle::Primary,
            source,
            span,
            message: String::new(),
        }
    }

    pub fn secondary(source: SourceId, span: Span) -> Self {
        Self {
            style: LabelStyle::Secondary,
            ..Self::primary(source, span)
        }
    }

    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoteKind {
    Note,
    Help,
}

/// A trailing `= note:` or `= help:` line, not tied to a source range.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub kind: NoteKind,
    pub message: String,
}

/// One report: a severity, an optional code, a message, labelled source
/// ranges, and notes. Messages are owned strings: diagnostics are built only
/// on failure, so formatting cost stays off the success path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<Code>,
    pub message: String,
    pub labels: Vec<Label>,
    pub notes: Vec<Note>,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: impl Into<String>) -> Self {
        Self {
            severity,
            code: None,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn error(code: Code, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, message).with_code(code)
    }

    pub fn warning(code: Code, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, message).with_code(code)
    }

    pub fn fault(message: impl Into<String>) -> Self {
        Self::new(Severity::Fault, message)
    }

    pub fn bug(message: impl Into<String>) -> Self {
        Self::new(Severity::Bug, message)
    }

    pub fn with_code(mut self, code: Code) -> Self {
        self.code = Some(code);
        self
    }

    pub fn with_label(mut self, label: Label) -> Self {
        self.labels.push(label);
        self
    }

    pub fn with_note(mut self, message: impl Into<String>) -> Self {
        self.notes.push(Note {
            kind: NoteKind::Note,
            message: message.into(),
        });
        self
    }

    pub fn with_help(mut self, message: impl Into<String>) -> Self {
        self.notes.push(Note {
            kind: NoteKind::Help,
            message: message.into(),
        });
        self
    }

    /// The first primary label, which locates the diagnostic as a whole.
    pub fn primary(&self) -> Option<&Label> {
        self.labels
            .iter()
            .find(|label| label.style == LabelStyle::Primary)
    }
}

/// An ordered collection of diagnostics with a cap, for producers that keep
/// going after an error (T047 recovery). Diagnostics past the cap are
/// counted, not stored, so a pathological input cannot grow memory without
/// bound.
#[derive(Clone, Debug)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
    limit: usize,
    errors: usize,
    dropped: usize,
}

impl Diagnostics {
    pub fn new(limit: usize) -> Self {
        Self {
            items: Vec::new(),
            limit,
            errors: 0,
            dropped: 0,
        }
    }

    pub fn push(&mut self, diagnostic: Diagnostic) {
        if diagnostic.severity >= Severity::Error {
            self.errors += 1;
        }
        if self.items.len() < self.limit {
            self.items.push(diagnostic);
        } else {
            self.dropped += 1;
        }
    }

    /// True once the cap is reached; producers may stop recovering.
    pub fn is_full(&self) -> bool {
        self.items.len() >= self.limit
    }

    pub fn has_errors(&self) -> bool {
        self.errors > 0
    }

    /// Diagnostics reported past the cap.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.items.iter()
    }

    /// Empty the collection, keeping its allocation for the next run.
    pub fn clear(&mut self) {
        self.items.clear();
        self.errors = 0;
        self.dropped = 0;
    }

    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.items
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type Item = &'a Diagnostic;
    type IntoIter = std::slice::Iter<'a, Diagnostic>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}
