//! The lexer: source text to a compact token buffer, in one pass.
//!
//! It tokenizes the whole lexical grammar, not just the first slice, so the
//! parser can reject an unsupported feature (a float, a string, a lambda)
//! with one deliberate diagnostic at the right span instead of a cascade.
//!
//! - Identifiers are interned as they are scanned; a keyword is an
//!   identifier whose symbol falls in the predefined keyword range, so
//!   keyword recognition costs nothing beyond the interning.
//! - Delimiters are matched with a stack and linked both ways in the token
//!   payloads. Unmatched and mismatched delimiters are reported here.
//! - `NEWLINE` tokens follow [Lexical structure: Newlines]: one at a line
//!   break when the innermost delimiter is `{` or none and the previous token
//!   is not a continuation token; consecutive breaks collapse. A line break
//!   inside a block comment counts.
//! - String literals (plain, multi-line, raw, with nested `${…}`) and char
//!   literals become single tokens spanning the whole literal; escapes are
//!   validated when literals are decoded, not here.
//! - Integer and float spellings are validated here but not converted:
//!   their value depends on the expected type.
//!
//! Errors are reported and lexing continues, so one run reports every
//! lexical error. Time is linear in the source; memory is the token buffer
//! (13 bytes per token), the delimiter stack, and new spellings.
//!
//! [Lexical structure: Newlines]: ../../../docs/language/lexical-structure.md#newlines

use eitri_base::{Interner, SourceId, Span};
use eitri_diag::{Code, Diagnostic, Diagnostics, Label};

use crate::token::{TokenId, TokenKind, Tokens};

/// Deepest delimiter nesting accepted (`E0002` beyond it), from the
/// [first slice](../../../docs/implementation/first-slice.md#resources).
pub const MAX_NESTING: usize = 256;

/// String-scanning state: inside a literal, or inside a `${…}` in one.
#[derive(Clone, Copy)]
enum Frame {
    Str { triple: bool },
    Interpolation { braces: u32 },
}

/// The lexer's reusable working memory: the token buffer it fills and the
/// stacks it scans with. Kept by the caller's context and cleared at the
/// start of each [`lex`], so warm runs allocate nothing (E15).
#[derive(Default)]
pub struct LexScratch {
    tokens: Tokens,
    delimiters: Vec<TokenId>,
    strings: Vec<Frame>,
}

impl LexScratch {
    pub fn new() -> Self {
        Self::default()
    }

    /// The tokens of the last [`lex`] into this scratch.
    pub fn tokens(&self) -> &Tokens {
        &self.tokens
    }

    /// Take the tokens out, e.g. to keep them past the next run.
    pub fn into_tokens(self) -> Tokens {
        self.tokens
    }

    /// Heap bytes retained, for scratch high-water accounting.
    pub fn heap_bytes(&self) -> usize {
        self.tokens.heap_bytes()
            + self.delimiters.capacity() * size_of::<TokenId>()
            + self.strings.capacity() * size_of::<Frame>()
    }
}

/// Lex `text` into `scratch`, replacing its previous tokens, and return them.
/// The last token is always [`TokenKind::Eof`]. Spellings are interned into
/// `interner`; errors are pushed to `diagnostics` with spans into `source`.
pub fn lex<'s>(
    text: &str,
    source: SourceId,
    interner: &mut Interner,
    diagnostics: &mut Diagnostics,
    scratch: &'s mut LexScratch,
) -> &'s Tokens {
    scratch.tokens.clear();
    scratch.delimiters.clear();
    scratch.strings.clear();
    lex_into(
        text,
        source,
        interner,
        diagnostics,
        &mut scratch.tokens,
        &mut scratch.delimiters,
        &mut scratch.strings,
    );
    &scratch.tokens
}

fn lex_into(
    text: &str,
    source: SourceId,
    interner: &mut Interner,
    diagnostics: &mut Diagnostics,
    tokens: &mut Tokens,
    delimiters: &mut Vec<TokenId>,
    strings: &mut Vec<Frame>,
) {
    if u32::try_from(text.len()).is_err() {
        diagnostics.push(error(
            Code::Limit,
            "implementation limit: source is 4 GiB or larger",
            source,
            Span::empty(0),
        ));
        tokens.push(TokenKind::Eof, Span::empty(0), 0);
        return;
    }
    // Short scripts average several bytes per token; one reservation
    // usually covers the whole buffer.
    tokens.reserve(text.len() / 4 + 1);
    let mut scan = Scan {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        source,
        tokens,
        interner,
        diagnostics,
        delimiters,
        strings,
        nesting_reported: false,
        swallowed: None,
    };
    scan.run();
}

fn error(code: Code, message: impl Into<String>, source: SourceId, span: Span) -> Diagnostic {
    Diagnostic::error(code, message).with_label(Label::primary(source, span))
}

struct Scan<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    source: SourceId,
    tokens: &'a mut Tokens,
    interner: &'a mut Interner,
    diagnostics: &'a mut Diagnostics,
    /// Open delimiters, innermost last.
    delimiters: &'a mut Vec<TokenId>,
    /// String and interpolation frames while scanning one literal.
    strings: &'a mut Vec<Frame>,
    nesting_reported: bool,
    /// Byte range where closers may have been swallowed by the first
    /// unterminated literal or comment: openers starting in it are not
    /// reported as unclosed, since their closer is likely inside it.
    swallowed: Option<(usize, usize)>,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl Scan<'_> {
    #[inline]
    fn peek(&self, offset: usize) -> u8 {
        self.bytes.get(self.pos + offset).copied().unwrap_or(0)
    }

    #[inline]
    fn span(&self, start: usize, end: usize) -> Span {
        // `lex` checked that every offset fits in `u32`.
        Span::new(start as u32, end as u32)
    }

    fn report(&mut self, code: Code, message: impl Into<String>, start: usize, end: usize) {
        let span = self.span(start, end);
        self.diagnostics
            .push(error(code, message, self.source, span));
    }

    #[inline]
    fn push(&mut self, kind: TokenKind, start: usize, payload: u32) -> TokenId {
        let span = self.span(start, self.pos);
        self.tokens.push(kind, span, payload)
    }

    fn run(&mut self) {
        loop {
            // Most tokens are followed by exactly one space. Step over it
            // without a branch, so it never costs a trip through the
            // (unpredictable) dispatch below; longer runs still go there.
            self.pos += usize::from(self.bytes.get(self.pos) == Some(&b' '));
            let Some(&b) = self.bytes.get(self.pos) else {
                break;
            };
            let start = self.pos;
            match b {
                // Indentation comes in runs; take it eight bytes at a time.
                b' ' => self.pos = swar::spaces_end(self.bytes, start + 1),
                b'\t' | b'\r' => self.pos += 1,
                b'\n' => {
                    self.line_break(start);
                    self.pos += 1;
                }
                b'/' if self.peek(1) == b'/' => {
                    self.pos = swar::find_newline(self.bytes, start + 2)
                }
                b'/' if self.peek(1) == b'*' => self.block_comment(),
                b'0'..=b'9' => self.number(),
                b'"' => self.string(false),
                b'\'' => self.char_literal(),
                b'@' => self.attribute(),
                _ if is_ident_start(b) => self.word(),
                _ => {
                    if let Some(kind) = self.punct(b) {
                        self.delimiter_or_token(kind, start);
                    } else {
                        let c = self.text[start..].chars().next().unwrap();
                        self.pos += c.len_utf8();
                        let shown = if c.is_control() {
                            c.escape_default().to_string()
                        } else {
                            c.to_string()
                        };
                        self.report(
                            Code::Lexical,
                            format!("unexpected character `{shown}`"),
                            start,
                            self.pos,
                        );
                    }
                }
            }
        }

        // Every opener still on the stack is unclosed, reported in source
        // order, except those an unterminated literal may have swallowed.
        for i in 0..self.delimiters.len() {
            let open = self.delimiters[i];
            let at = self.tokens.span(open).start as usize;
            if !self
                .swallowed
                .is_some_and(|(from, to)| (from..to).contains(&at))
            {
                self.unclosed(open);
            }
        }
        self.delimiters.clear();
        let end = self.bytes.len();
        self.tokens.push(TokenKind::Eof, self.span(end, end), 0);
    }

    /// Record the first unterminated literal or comment starting at `start`.
    /// A single-line one can only have swallowed closers on its own line;
    /// a multi-line one, any closer after any earlier opener.
    fn swallowing(&mut self, start: usize, multi_line: bool) {
        if self.swallowed.is_some() {
            return;
        }
        let from = if multi_line {
            0
        } else {
            self.bytes[..start]
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |newline| newline + 1)
        };
        self.swallowed = Some((from, start));
    }

    /// Emit a `NEWLINE` for a line break at `at`, if the rules call for one.
    fn line_break(&mut self, at: usize) {
        let in_braces = self
            .delimiters
            .last()
            .is_none_or(|&open| self.tokens.kind(open) == TokenKind::LBrace);
        let ends_statement = match self.tokens.last_kind() {
            None | Some(TokenKind::Newline) => false,
            Some(kind) => !kind.continues_line(),
        };
        if in_braces && ends_statement {
            let span = self.span(at, at + 1);
            self.tokens.push(TokenKind::Newline, span, 0);
        }
    }

    fn block_comment(&mut self) {
        let start = self.pos;
        // Jump from `*` to `*`, eight bytes at a time, until one closes.
        let mut at = start + 2;
        let closed = loop {
            at = swar::find_either(self.bytes, at, b'*', b'*');
            match self.bytes.get(at + 1) {
                Some(b'/') => break true,
                Some(_) => at += 1,
                None => break false,
            }
        };
        let end = if closed { at + 2 } else { self.bytes.len() };
        // A line break inside the comment still separates statements.
        let newline = swar::find_newline(self.bytes, start + 2);
        if newline < end {
            self.line_break(newline);
        }
        self.pos = end;
        if !closed {
            self.report(Code::Lexical, "unterminated block comment", start, end);
            self.swallowing(start, true);
        }
    }

    /// An identifier, keyword, `_`, `try?`, or raw string `r"…"`.
    fn word(&mut self) {
        let start = self.pos;
        let len = swar::ident_end(self.bytes, start + 1) - start;
        let word = &self.text[start..start + len];
        if word == "r" && self.peek(1) == b'"' {
            self.pos += 1;
            return self.string(true);
        }
        self.pos += len;
        if word == "_" {
            self.push(TokenKind::Wildcard, start, 0);
            return;
        }
        let Ok(symbol) = self.interner.try_intern(word) else {
            self.report(
                Code::Limit,
                "implementation limit: too many distinct names",
                start,
                self.pos,
            );
            return;
        };
        let kind = match TokenKind::keyword(symbol) {
            Some(TokenKind::Try) if self.peek(0) == b'?' => {
                self.pos += 1;
                TokenKind::TryQuestion
            }
            Some(kind) => kind,
            None => TokenKind::Ident,
        };
        self.push(kind, start, symbol.as_u32());
    }

    fn attribute(&mut self) {
        let start = self.pos;
        self.pos += 1;
        if !is_ident_start(self.peek(0)) {
            self.report(Code::Lexical, "unexpected character `@`", start, self.pos);
            return;
        }
        let name_start = self.pos;
        while is_ident_continue(self.peek(0)) {
            self.pos += 1;
        }
        let name = &self.text[name_start..self.pos];
        match self.interner.try_intern(name) {
            Ok(symbol) => {
                self.push(TokenKind::Attribute, start, symbol.as_u32());
            }
            Err(_) => self.report(
                Code::Limit,
                "implementation limit: too many distinct names",
                start,
                self.pos,
            ),
        }
    }

    /// Integer and float literals. The spelling is validated; anything that
    /// runs on with identifier characters makes the whole run malformed.
    fn number(&mut self) {
        let start = self.pos;
        let mut kind = TokenKind::Int;
        let mut valid = true;
        let radix = match (self.peek(0), self.peek(1)) {
            (b'0', b'x') => 16,
            (b'0', b'o') => 8,
            (b'0', b'b') => 2,
            _ => 10,
        };
        let digit = |b: u8| (b as char).is_digit(radix);
        if radix != 10 {
            self.pos += 2;
            valid = digit(self.peek(0));
        }
        while digit(self.peek(0)) || self.peek(0) == b'_' {
            self.pos += 1;
        }
        if radix == 10 {
            // A digit must follow `.`, so `0..n` stays a range.
            if self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
                kind = TokenKind::Float;
                self.pos += 1;
                while self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
                    self.pos += 1;
                }
            }
            if matches!(self.peek(0), b'e' | b'E') {
                let sign = usize::from(matches!(self.peek(1), b'+' | b'-'));
                if self.peek(1 + sign).is_ascii_digit() {
                    kind = TokenKind::Float;
                    self.pos += 1 + sign;
                    while self.peek(0).is_ascii_digit() || self.peek(0) == b'_' {
                        self.pos += 1;
                    }
                }
            }
        }
        if is_ident_continue(self.peek(0)) {
            valid = false;
            while is_ident_continue(self.peek(0)) {
                self.pos += 1;
            }
        }
        if !valid {
            let what = if kind == TokenKind::Float {
                "float"
            } else {
                "integer"
            };
            self.report(
                Code::Lexical,
                format!("malformed {what} literal"),
                start,
                self.pos,
            );
        }
        // Push even when malformed, so the parser sees a literal and does
        // not report follow-on errors.
        self.push(kind, start, 0);
    }

    /// A string literal from `self.pos` (its opening quote), including any
    /// nested interpolations and the strings inside them.
    fn string(&mut self, raw: bool) {
        let start = if raw { self.pos - 1 } else { self.pos };
        let triple = self.bytes[self.pos..].starts_with(b"\"\"\"");
        self.pos += if triple { 3 } else { 1 };
        if triple {
            let rest = &self.bytes[self.pos..];
            let line_end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
            if !rest[..line_end]
                .iter()
                .all(|&b| b == b' ' || b == b'\t' || b == b'\r')
            {
                self.report(
                    Code::Lexical,
                    "a multi-line string starts on the line after `\"\"\"`",
                    start,
                    self.pos,
                );
            }
        }

        let terminated = if raw {
            self.raw_body(triple)
        } else {
            self.strings.clear();
            self.strings.push(Frame::Str { triple });
            self.string_body()
        };
        if !terminated {
            self.report(
                Code::Lexical,
                "unterminated string literal",
                start,
                self.pos,
            );
            self.swallowing(start, triple);
        }
        self.push(TokenKind::Str, start, 0);
    }

    /// Scan to the end of a raw string. On failure, `pos` is where it stops.
    fn raw_body(&mut self, triple: bool) -> bool {
        // No escapes or interpolation: only a quote or, in a single-line
        // literal, a line break matters. Jump to it eight bytes at a time.
        let stop = if triple { b'"' } else { b'\n' };
        loop {
            self.pos = swar::find_either(self.bytes, self.pos, b'"', stop);
            match self.bytes.get(self.pos) {
                None | Some(b'\n') => return false,
                Some(_) if !triple => {
                    self.pos += 1;
                    return true;
                }
                Some(_) if self.bytes[self.pos..].starts_with(b"\"\"\"") => {
                    self.pos += 3;
                    return true;
                }
                Some(_) => self.pos += 1,
            }
        }
    }

    /// Scan through the frames on the string stack until it empties. Uses an
    /// explicit stack, so deeply nested interpolations cannot overflow the
    /// Rust stack.
    fn string_body(&mut self) -> bool {
        while let Some(&frame) = self.strings.last() {
            let Some(&b) = self.bytes.get(self.pos) else {
                return false;
            };
            match frame {
                Frame::Str { triple } => match b {
                    // `\` never escapes a line break: that stays unterminated.
                    b'\\' if self.peek(1) != b'\n' => {
                        self.pos += 2.min(self.bytes.len() - self.pos)
                    }
                    b'"' if !triple => {
                        self.pos += 1;
                        self.strings.pop();
                    }
                    b'"' if self.bytes[self.pos..].starts_with(b"\"\"\"") => {
                        self.pos += 3;
                        self.strings.pop();
                    }
                    b'\n' if !triple => return false,
                    b'$' if self.peek(1) == b'{' => {
                        self.pos += 2;
                        self.strings.push(Frame::Interpolation { braces: 0 });
                    }
                    // Nothing before the next `"`, `\`, `$`, or line break
                    // can change state: skip to it, eight bytes at a time.
                    _ => self.pos = swar::find_string_special(self.bytes, self.pos + 1),
                },
                Frame::Interpolation { braces } => {
                    let top = self.strings.len() - 1;
                    match b {
                        b'{' => self.strings[top] = Frame::Interpolation { braces: braces + 1 },
                        b'}' if braces == 0 => {
                            self.strings.pop();
                        }
                        b'}' => self.strings[top] = Frame::Interpolation { braces: braces - 1 },
                        b'"' => {
                            let triple = self.bytes[self.pos..].starts_with(b"\"\"\"");
                            self.strings.push(Frame::Str { triple });
                            self.pos += if triple { 2 } else { 0 };
                        }
                        b'\'' => {
                            self.pos = self.char_end(self.pos).unwrap_or(self.pos + 1);
                            continue;
                        }
                        b'\n' if self.in_single_line_string() => return false,
                        _ => {}
                    }
                    self.pos += 1;
                }
            }
        }
        true
    }

    fn in_single_line_string(&self) -> bool {
        self.strings
            .iter()
            .any(|frame| matches!(frame, Frame::Str { triple: false }))
    }

    /// The end of a well-formed char literal starting at `start`, if it is one.
    fn char_end(&self, start: usize) -> Option<usize> {
        let mut i = start + 1;
        match *self.bytes.get(i)? {
            b'\\'
                if self.bytes.get(i + 1) == Some(&b'u') && self.bytes.get(i + 2) == Some(&b'{') =>
            {
                i += self.bytes[i..]
                    .iter()
                    .position(|&b| b == b'}' || b == b'\n')?;
                if self.bytes[i] != b'}' {
                    return None;
                }
                i += 1;
            }
            b'\\' => i += 1 + self.text[i + 1..].chars().next()?.len_utf8(),
            b'\'' | b'\n' => return None,
            _ => i += self.text[i..].chars().next()?.len_utf8(),
        }
        (self.bytes.get(i) == Some(&b'\'')).then_some(i + 1)
    }

    fn char_literal(&mut self) {
        let start = self.pos;
        if let Some(end) = self.char_end(start) {
            self.pos = end;
        } else {
            // Recover at the next quote on the line, or the line's end.
            let rest = &self.bytes[start + 1..];
            let stop = rest.iter().position(|&b| b == b'\'' || b == b'\n');
            match stop {
                Some(i) if rest[i] == b'\'' => {
                    self.pos = start + 1 + i + 1;
                    self.report(Code::Lexical, "malformed char literal", start, self.pos);
                }
                _ => {
                    self.pos = start + 1 + stop.unwrap_or(rest.len());
                    self.report(Code::Lexical, "unterminated char literal", start, self.pos);
                    self.swallowing(start, false);
                }
            }
        }
        self.push(TokenKind::Char, start, 0);
    }

    /// The punctuation token at `pos`, by longest match, advancing past it.
    fn punct(&mut self, b: u8) -> Option<TokenKind> {
        use TokenKind::*;
        let (kind, len) = match (b, self.peek(1), self.peek(2)) {
            (b'(', _, _) => (LParen, 1),
            (b')', _, _) => (RParen, 1),
            (b'[', _, _) => (LBracket, 1),
            (b']', _, _) => (RBracket, 1),
            (b'{', _, _) => (LBrace, 1),
            (b'}', _, _) => (RBrace, 1),
            (b',', _, _) => (Comma, 1),
            (b';', _, _) => (Semi, 1),
            (b':', _, _) => (Colon, 1),
            (b'.', b'.', b'=') => (DotDotEq, 3),
            (b'.', b'.', _) => (DotDot, 2),
            (b'.', _, _) => (Dot, 1),
            (b'?', b'.', _) => (QuestionDot, 2),
            (b'?', b'[', _) => (QuestionBracket, 2),
            (b'?', b'?', _) => (QuestionQuestion, 2),
            (b'?', _, _) => (Question, 1),
            (b'=', b'>', _) => (FatArrow, 2),
            (b'=', b'=', _) => (EqEq, 2),
            (b'=', _, _) => (Eq, 1),
            (b'-', b'>', _) => (Arrow, 2),
            (b'-', b'=', _) => (MinusEq, 2),
            (b'-', _, _) => (Minus, 1),
            (b'!', b'=', _) => (NotEq, 2),
            (b'!', _, _) => (Bang, 1),
            (b'<', b'<', b'=') => (ShlEq, 3),
            (b'<', b'<', _) => (Shl, 2),
            (b'<', b'=', _) => (LtEq, 2),
            (b'<', _, _) => (Lt, 1),
            // `>` is always single: the parser joins adjacent `>` tokens.
            (b'>', b'=', _) => (GtEq, 2),
            (b'>', _, _) => (Gt, 1),
            (b'&', b'&', _) => (AndAnd, 2),
            (b'&', b'+', b'=') => (WrapAddEq, 3),
            (b'&', b'+', _) => (WrapAdd, 2),
            (b'&', b'-', b'=') => (WrapSubEq, 3),
            (b'&', b'-', _) => (WrapSub, 2),
            (b'&', b'*', b'=') => (WrapMulEq, 3),
            (b'&', b'*', _) => (WrapMul, 2),
            (b'&', b'=', _) => (AmpEq, 2),
            (b'&', _, _) => (Amp, 1),
            (b'|', b'|', _) => (OrOr, 2),
            (b'|', b'=', _) => (PipeEq, 2),
            (b'|', _, _) => (Pipe, 1),
            (b'^', b'=', _) => (CaretEq, 2),
            (b'^', _, _) => (Caret, 1),
            (b'+', b'=', _) => (PlusEq, 2),
            (b'+', _, _) => (Plus, 1),
            (b'*', b'=', _) => (StarEq, 2),
            (b'*', _, _) => (Star, 1),
            (b'/', b'=', _) => (SlashEq, 2),
            (b'/', _, _) => (Slash, 1),
            (b'%', b'=', _) => (PercentEq, 2),
            (b'%', _, _) => (Percent, 1),
            (b'~', _, _) => (Tilde, 1),
            _ => return None,
        };
        self.pos += len;
        Some(kind)
    }

    fn delimiter_or_token(&mut self, kind: TokenKind, start: usize) {
        if kind.is_open_delimiter() {
            let token = self.push(kind, start, 0);
            self.tokens.mark_unmatched(token);
            if self.delimiters.len() == MAX_NESTING && !self.nesting_reported {
                self.nesting_reported = true;
                self.report(
                    Code::Limit,
                    format!("implementation limit: delimiters nested deeper than {MAX_NESTING}"),
                    start,
                    self.pos,
                );
            }
            self.delimiters.push(token);
            return;
        }
        if !matches!(
            kind,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace
        ) {
            self.push(kind, start, 0);
            return;
        }

        let token = self.push(kind, start, 0);
        self.tokens.mark_unmatched(token);
        let opens = |tokens: &Tokens, open: TokenId| tokens.kind(open).closer() == Some(kind);
        let Some(depth) = self
            .delimiters
            .iter()
            .rposition(|&open| opens(self.tokens, open))
        else {
            self.report(
                Code::Syntax,
                format!("unexpected closing delimiter {}", kind.describe()),
                start,
                self.pos,
            );
            return;
        };
        // Openers above the match were never closed.
        for i in depth + 1..self.delimiters.len() {
            let open = self.delimiters[i];
            self.unclosed(open);
        }
        let open = self.delimiters[depth];
        self.delimiters.truncate(depth);
        self.tokens.link(open, token);
    }

    fn unclosed(&mut self, open: TokenId) {
        let kind = self.tokens.kind(open);
        let span = self.tokens.span(open);
        self.diagnostics.push(
            Diagnostic::error(
                Code::Syntax,
                format!("unclosed delimiter {}", kind.describe()),
            )
            .with_label(Label::primary(self.source, span).with_message("never closed")),
        );
    }
}

mod swar;

#[cfg(test)]
mod reference;
#[cfg(test)]
mod tests;
