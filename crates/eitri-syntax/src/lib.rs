//! The Eitri frontend's syntax layer: tokens and the lexer (T007), the
//! syntax tree, and the parser (T008).

pub mod ast;
mod lexer;
mod parser;
mod token;

#[cfg(test)]
mod test_support;

pub use lexer::{LexScratch, MAX_NESTING, lex};
pub use parser::{ModuleKind, ParseScratch, parse};
pub use token::{TokenId, TokenKind, Tokens};
