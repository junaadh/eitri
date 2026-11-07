//! Eitri implementation bootstrap. This API is experimental and unstable.

mod context;

pub use context::Context;
/// The shared diagnostic model and renderer, re-exported so hosts need no
/// separate dependency.
pub use eitri_diag as diag;
