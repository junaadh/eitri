use eitri_diag::{Code, Diagnostic, Label, SourceId, Span};

/// Caller-owned execution context, borrowed exclusively during submission.
///
/// Future compiler scratch and runtime state belong here, with each layer
/// borrowing only the state it needs. No global state or synchronization.
#[derive(Default)]
pub struct Context {
    // No compiler/runtime state exists at bootstrap. Keep fields private.
}

impl Context {
    /// The id diagnostics use for the submitted source. Register that source
    /// first in the [`eitri_diag::Files`] used for rendering.
    pub const SOURCE: SourceId = SourceId::from_u32(0);

    pub fn new() -> Self {
        Self::default()
    }

    /// Submit UTF-8 source. No source is executable during the bootstrap stage.
    /// Even empty input is rejected; success must mean checked execution.
    pub fn run(&mut self, source: &str) -> Result<(), Diagnostic> {
        let first = source.chars().next().map_or(0, char::len_utf8) as u32;
        Err(
            Diagnostic::error(Code::new(1), "source execution is not implemented")
                .with_label(Label::primary(Self::SOURCE, Span::new(0, first))),
        )
    }
}
