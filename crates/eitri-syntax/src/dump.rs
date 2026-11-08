//! The token stream as text, for inspection (`eitri dump tokens`). Not a
//! stable format. The syntax tree's dump needs the checker's types, so it
//! lives in `eitri-check`.

use std::fmt::Write;

use eitri_base::{Idx, Span};

use crate::token::{TokenId, Tokens};

/// One token per line: byte range, kind, and text.
pub fn tokens(text: &str, tokens: &Tokens) -> String {
    let mut out = String::new();
    for i in 0..tokens.len() {
        let id = TokenId::new(i);
        let span = tokens.span(id);
        let range = format!("{}..{}", span.start, span.end);
        let kind = format!("{:?}", tokens.kind(id));
        let shown = slice(text, span).replace('\n', "\\n").replace('\t', "\\t");
        writeln!(out, "{range:<12}{kind:<16}{shown}").unwrap();
    }
    out
}

fn slice(text: &str, span: Span) -> &str {
    text.get(span.start as usize..span.end as usize)
        .unwrap_or("")
}
