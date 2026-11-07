//! The caller-owned source registry used for rendering.

use eitri_base::{IndexVec, SourceId};

/// A source registered for rendering.
#[derive(Clone, Copy, Debug)]
pub struct File<'a> {
    pub name: &'a str,
    pub text: &'a str,
}

/// Caller-owned registry of the sources diagnostics refer to. It borrows the
/// text; the compiler keeps owning its sources.
#[derive(Clone, Debug, Default)]
pub struct Files<'a> {
    files: IndexVec<SourceId, File<'a>>,
}

impl<'a> Files<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a source. Ids are assigned densely from zero, in call order.
    pub fn add(&mut self, name: &'a str, text: &'a str) -> SourceId {
        self.files.push(File { name, text })
    }

    pub fn get(&self, id: SourceId) -> Option<File<'a>> {
        self.files.get(id).copied()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// A 1-based line and column. The column counts Unicode scalar values, as
/// rustc does, so it is stable across terminals and tab settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Location {
    pub line: u32,
    pub column: u32,
}
