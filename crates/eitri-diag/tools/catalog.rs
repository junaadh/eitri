//! Shared by the explicit documentation generator and its read-only test.

use std::fmt::Write;

use eitri_diag::Code;

pub const START: &str = "<!-- BEGIN GENERATED DIAGNOSTIC CATALOG -->";
pub const END: &str = "<!-- END GENERATED DIAGNOSTIC CATALOG -->";

pub fn table() -> String {
    let mut text = format!("{START}\n\n| Code | Meaning |\n|---|---|\n");
    for code in Code::ALL {
        writeln!(text, "| `{code}` | {} |", code.summary()).unwrap();
    }
    write!(text, "\n{END}").unwrap();
    text
}

/// Replace only the marked generated section; fail on missing/duplicate markers.
pub fn replace(document: &str) -> Result<String, &'static str> {
    if document.matches(START).count() != 1 || document.matches(END).count() != 1 {
        return Err("expected exactly one diagnostic catalog marker pair");
    }
    let start = document.find(START).unwrap();
    let end = document.find(END).unwrap();
    if end < start {
        return Err("diagnostic catalog markers are reversed");
    }
    Ok(format!(
        "{}{}{}",
        &document[..start],
        table(),
        &document[end + END.len()..]
    ))
}
