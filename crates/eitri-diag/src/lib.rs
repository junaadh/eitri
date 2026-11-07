//! Structured diagnostics shared by every Eitri layer, and a renderer for
//! them in the codespan-reporting layout.
//!
//! Producers (lexer, parser, checker, verifier, VM) build [`Diagnostic`]
//! values from cheap [`Span`]s and never touch line tables or text layout.
//! Line/column resolution and excerpt rendering happen only in [`Renderer`],
//! on the cold reporting path, so valid programs pay nothing for them.
//!
//! The crate has no global state: sources are registered in a caller-owned
//! [`Files`] and the renderer's line cache lives in the renderer.

mod codes;
mod diagnostic;
mod files;
mod render;
mod width;

pub use codes::Code;
pub use diagnostic::{Diagnostic, Diagnostics, Label, LabelStyle, Note, NoteKind, Severity};
pub use eitri_base::{SourceId, Span};
pub use files::{File, Files, Location};
pub use render::{Chars, DisplayStyle, RenderOptions, Renderer, Styles};
