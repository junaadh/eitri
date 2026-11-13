//! The Eitri frontend's syntax layer: tokens and the lexer (T007), the
//! syntax tree, and the parser (T008).

pub mod ast;
pub mod dump;
mod lexer;
mod parser;
mod token;

#[cfg(test)]
mod test_support;

pub use lexer::{LexScratch, MAX_NESTING, lex, lex_budgeted};
pub use parser::{ModuleKind, ParseScratch, parse, parse_budgeted};
pub use token::{TokenId, TokenKind, Tokens};
