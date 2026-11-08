//! Parsing `.eits` into functions, through the [`Assembler`].
//!
//! Two passes over the tokens: the first collects every `declare` and
//! `define` header, so calls may refer to functions defined later; the
//! second assembles each body. Linear in the size of the text.

use eitri_base::{FxHashMap, Idx, Span};

use super::{HostDecl, MODULE, mnemonic};
use crate::asm::{Assembler, imm_complement};
use crate::encoding::{EncodeError, encode};
use crate::encoding::{MAX_SLOT, Operands};
use crate::function::{
    CallSite, CallTarget, Function, FunctionId, Signature, SlotRange, ValueType,
};
use crate::ir::{self, Branch, Insn, Item};
use crate::spec::{OPS, Op, Role, Sem};

/// Functions in definition order, their logical instructions, and the host
/// functions they declare. Host call sites index `hosts`; spans are byte
/// ranges of the text.
#[derive(Debug)]
pub struct Parsed {
    pub functions: Vec<Function>,
    /// Each function's body as assembled ([`ir`]), positions filled in.
    pub bodies: Vec<ir::Body>,
    pub hosts: Vec<HostDecl>,
    /// The span of each function's name in its `define`.
    pub function_spans: Vec<Span>,
    /// The span of each host function's name in its `declare`.
    pub host_spans: Vec<Span>,
}

/// Why text did not assemble. `mismatch` distinguishes text that parses but
/// contradicts its program (`E0902`) from malformed text (`E0901`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextError {
    pub span: Span,
    pub message: String,
    pub mismatch: bool,
}

type Result<T> = std::result::Result<T, TextError>;

fn syntax(span: Span, message: impl Into<String>) -> TextError {
    TextError {
        span,
        message: message.into(),
        mismatch: false,
    }
}

fn mismatch(span: Span, message: impl Into<String>) -> TextError {
    TextError {
        span,
        message: message.into(),
        mismatch: true,
    }
}

/// Parse and assemble `text`.
pub fn parse(text: &str) -> Result<Parsed> {
    let tokens = lex(text)?;
    let mut parser = Parser {
        text,
        tokens: &tokens,
        at: 0,
    };
    let headers = parser.headers()?;
    let mut functions = Vec::new();
    let mut bodies = Vec::new();
    let mut asm = Assembler::recording();
    let mut words = Vec::new();
    let mnemonics: FxHashMap<String, Op> = (0..OPS.len())
        .filter_map(|byte| Op::from_byte(byte as u8))
        .map(|op| (mnemonic(op), op))
        .collect();
    for define in &headers.defines {
        parser.at = define.body;
        let mut body = Body {
            parser: &mut parser,
            words: &mut words,
            headers: &headers,
            mnemonics: &mnemonics,
            items: Vec::new(),
            labels: FxHashMap::default(),
            constants: FxHashMap::default(),
        };
        let (function, assembled) = body.function(define, &mut asm)?;
        functions.push(function);
        bodies.push(assembled);
    }
    let (hosts, host_spans) = headers.hosts.into_iter().unzip();
    Ok(Parsed {
        functions,
        bodies,
        hosts,
        function_spans: headers.defines.iter().map(|d| d.name_span).collect(),
        host_spans,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok<'a> {
    /// An identifier; dots allowed, so mnemonics and host names are one.
    Word(&'a str),
    /// `@name`, `@<module>`, or `@"quoted"`.
    Global(&'a str),
    /// `%r` and a number.
    Slot(u32),
    /// `%name`: a label.
    Local(&'a str),
    Int(i128),
    Punct(u8),
    End,
}

#[derive(Clone, Copy)]
struct Token<'a> {
    tok: Tok<'a>,
    span: Span,
}

fn lex(text: &str) -> Result<Vec<Token<'_>>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    let word_end = |mut j: usize| {
        while j < bytes.len()
            && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'.')
        {
            j += 1;
        }
        j
    };
    while i < bytes.len() {
        let start = i;
        let c = bytes[i];
        let tok = match c {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
                continue;
            }
            b';' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'(' | b')' | b'{' | b'}' | b',' | b':' | b'=' => {
                i += 1;
                Tok::Punct(c)
            }
            b'@' => {
                i += 1;
                if text[i..].starts_with(MODULE) {
                    i += MODULE.len();
                    Tok::Global(MODULE)
                } else if bytes.get(i) == Some(&b'"') {
                    let close = text[i + 1..]
                        .find(['"', '\n'])
                        .filter(|&n| bytes[i + 1 + n] == b'"')
                        .ok_or_else(|| {
                            let line_end = text[i..].find('\n').map_or(text.len(), |n| i + n);
                            syntax(span(start, line_end), "unterminated quoted name")
                        })?;
                    let name = &text[i + 1..i + 1 + close];
                    i += close + 2;
                    Tok::Global(name)
                } else {
                    let end = word_end(i);
                    if end == i {
                        return Err(syntax(span(start, i), "expected a name after `@`"));
                    }
                    let name = &text[i..end];
                    i = end;
                    Tok::Global(name)
                }
            }
            b'%' => {
                let end = word_end(i + 1);
                let name = &text[i + 1..end];
                i = end;
                match name
                    .strip_prefix('r')
                    .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                {
                    Some(number) => Tok::Slot(
                        number
                            .parse()
                            .ok()
                            .filter(|&slot: &u32| slot as i64 <= MAX_SLOT)
                            .ok_or_else(|| {
                                syntax(span(start, end), format!("slots go up to %r{MAX_SLOT}"))
                            })?,
                    ),
                    None if name.is_empty() => {
                        return Err(syntax(
                            span(start, end),
                            "expected a slot or label after `%`",
                        ));
                    }
                    None => Tok::Local(name),
                }
            }
            b'-' | b'0'..=b'9' => {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
                Tok::Int(
                    text[start..i]
                        .parse()
                        .map_err(|_| syntax(span(start, i), "malformed integer"))?,
                )
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                i = word_end(i);
                Tok::Word(&text[start..i])
            }
            _ => {
                let len = text[i..].chars().next().map_or(1, char::len_utf8);
                return Err(syntax(span(i, i + len), "unexpected character"));
            }
        };
        tokens.push(Token {
            tok,
            span: span(start, i),
        });
    }
    tokens.push(Token {
        tok: Tok::End,
        span: span(text.len(), text.len()),
    });
    Ok(tokens)
}

fn span(start: usize, end: usize) -> Span {
    Span::new(start as u32, end as u32)
}

struct Parser<'t, 'a> {
    text: &'a str,
    tokens: &'t [Token<'a>],
    at: usize,
}

struct Define<'a> {
    name: &'a str,
    name_span: Span,
    signature: Signature,
    /// Parameter slots as written, with their spans.
    params: Vec<(u32, Span)>,
    frame: (u32, Span),
    /// Token index of the first token after `{`.
    body: usize,
}

struct Headers<'a> {
    defines: Vec<Define<'a>>,
    functions: FxHashMap<&'a str, FunctionId>,
    hosts: Vec<(HostDecl, Span)>,
    host_index: FxHashMap<&'a str, u32>,
}

impl<'a> Parser<'_, 'a> {
    fn peek(&self) -> Token<'a> {
        self.tokens[self.at]
    }

    fn next(&mut self) -> Token<'a> {
        let token = self.tokens[self.at];
        if token.tok != Tok::End {
            self.at += 1;
        }
        token
    }

    fn expected(&self, what: &str) -> TextError {
        let token = self.peek();
        let found = match token.tok {
            Tok::End => "the end of the text".to_owned(),
            _ => format!(
                "`{}`",
                &self.text[token.span.start as usize..token.span.end as usize]
            ),
        };
        syntax(token.span, format!("expected {what}, found {found}"))
    }

    fn punct(&mut self, c: u8) -> Result<Span> {
        match self.peek().tok {
            Tok::Punct(p) if p == c => Ok(self.next().span),
            _ => Err(self.expected(&format!("`{}`", c as char))),
        }
    }

    fn eat_punct(&mut self, c: u8) -> bool {
        let found = self.peek().tok == Tok::Punct(c);
        if found {
            self.next();
        }
        found
    }

    fn word(&mut self, what: &str) -> Result<(&'a str, Span)> {
        match self.peek().tok {
            Tok::Word(w) => Ok((w, self.next().span)),
            _ => Err(self.expected(what)),
        }
    }

    fn keyword(&mut self, keyword: &str) -> Result<Span> {
        match self.peek().tok {
            Tok::Word(w) if w == keyword => Ok(self.next().span),
            _ => Err(self.expected(&format!("`{keyword}`"))),
        }
    }

    fn global(&mut self) -> Result<(&'a str, Span)> {
        match self.peek().tok {
            Tok::Global(name) => Ok((name, self.next().span)),
            _ => Err(self.expected("a name (`@name`)")),
        }
    }

    fn slot(&mut self) -> Result<(u32, Span)> {
        match self.peek().tok {
            Tok::Slot(slot) => Ok((slot, self.next().span)),
            _ => Err(self.expected("a slot (`%rN`)")),
        }
    }

    fn int(&mut self) -> Result<(i128, Span)> {
        match self.peek().tok {
            Tok::Int(value) => Ok((value, self.next().span)),
            _ => Err(self.expected("an integer")),
        }
    }

    /// `i64`, `bool`, or (when `void_ok`) `void` as `None`.
    fn ty(&mut self, void_ok: bool) -> Result<(Option<ValueType>, Span)> {
        let (word, span) = self.word("a type")?;
        match word {
            "i64" => Ok((Some(ValueType::I64), span)),
            "bool" => Ok((Some(ValueType::Bool), span)),
            "void" if void_ok => Ok((None, span)),
            _ => Err(syntax(span, format!("expected a type, found `{word}`"))),
        }
    }

    fn value_type(&mut self) -> Result<(ValueType, Span)> {
        match self.ty(false)? {
            (Some(ty), span) => Ok((ty, span)),
            (None, span) => Err(syntax(span, "expected `i64` or `bool`")),
        }
    }

    fn headers(&mut self) -> Result<Headers<'a>> {
        let mut headers = Headers {
            defines: Vec::new(),
            functions: FxHashMap::default(),
            hosts: Vec::new(),
            host_index: FxHashMap::default(),
        };
        loop {
            match self.peek().tok {
                Tok::End => return Ok(headers),
                Tok::Word("declare") => {
                    self.next();
                    let (ret, _) = self.ty(true)?;
                    let (name, name_span) = self.global()?;
                    self.punct(b'(')?;
                    let mut params = Vec::new();
                    if !self.eat_punct(b')') {
                        loop {
                            params.push(self.value_type()?.0);
                            if self.eat_punct(b')') {
                                break;
                            }
                            self.punct(b',')?;
                        }
                    }
                    if headers.host_index.contains_key(name) || headers.functions.contains_key(name)
                    {
                        return Err(mismatch(name_span, format!("`@{name}` is declared twice")));
                    }
                    headers.host_index.insert(name, headers.hosts.len() as u32);
                    headers.hosts.push((
                        HostDecl {
                            name: name.to_owned(),
                            signature: Signature {
                                params,
                                results: ret.into_iter().collect(),
                            },
                        },
                        name_span,
                    ));
                }
                Tok::Word("define") => {
                    self.next();
                    let (ret, _) = self.ty(true)?;
                    let (name, name_span) = self.global()?;
                    self.punct(b'(')?;
                    let mut types = Vec::new();
                    let mut params = Vec::new();
                    if !self.eat_punct(b')') {
                        loop {
                            types.push(self.value_type()?.0);
                            params.push(self.slot()?);
                            if self.eat_punct(b')') {
                                break;
                            }
                            self.punct(b',')?;
                        }
                    }
                    self.keyword("frame")?;
                    self.punct(b'(')?;
                    let (frame, frame_span) = self.int()?;
                    let frame = u32::try_from(frame)
                        .map_err(|_| syntax(frame_span, "frame size out of range"))?;
                    self.punct(b')')?;
                    self.punct(b'{')?;
                    let body = self.at;
                    // Skip the body; the second pass assembles it.
                    loop {
                        match self.next().tok {
                            Tok::Punct(b'}') => break,
                            Tok::End => {
                                return Err(syntax(
                                    name_span,
                                    format!("`@{name}` has no closing `}}`"),
                                ));
                            }
                            _ => {}
                        }
                    }
                    if headers.functions.contains_key(name) || headers.host_index.contains_key(name)
                    {
                        return Err(mismatch(name_span, format!("`@{name}` is defined twice")));
                    }
                    headers
                        .functions
                        .insert(name, FunctionId::new(headers.defines.len()));
                    headers.defines.push(Define {
                        name,
                        name_span,
                        signature: Signature {
                            params: types,
                            results: ret.into_iter().collect(),
                        },
                        params,
                        frame: (frame, frame_span),
                        body,
                    });
                }
                _ => return Err(self.expected("`declare` or `define`")),
            }
        }
    }
}

struct Body<'p, 't, 'a> {
    parser: &'p mut Parser<'t, 'a>,
    /// Scratch for checking that a plain instruction encodes.
    words: &'p mut Vec<u32>,
    headers: &'p Headers<'a>,
    mnemonics: &'p FxHashMap<String, Op>,
    /// The logical instructions so far.
    items: Vec<Item>,
    /// Each label: its number, where it was defined, and its first use.
    labels: FxHashMap<&'a str, (u32, Option<Span>, Span)>,
    /// Each distinct constant's pool index, in the assembler's order of
    /// first appearance, to know which load form it will choose.
    constants: FxHashMap<u64, usize>,
}

impl<'a> Body<'_, '_, 'a> {
    /// Parse the body, check it, and assemble it.
    fn function(
        &mut self,
        define: &Define<'a>,
        asm: &mut Assembler<true>,
    ) -> Result<(Function, ir::Body)> {
        let base = define.signature.param_base();
        for (i, &(slot, span)) in define.params.iter().enumerate() {
            if slot != base + i as u32 {
                return Err(mismatch(
                    span,
                    format!(
                        "parameter {i} is in slot %r{}, not %r{slot}",
                        base + i as u32
                    ),
                ));
            }
        }
        while !self.parser.eat_punct(b'}') {
            self.item()?;
        }
        for (name, &(_, bound, used)) in &self.labels {
            if bound.is_none() {
                return Err(mismatch(used, format!("label `%{name}` is never defined")));
            }
        }
        let body = ir::Body {
            items: std::mem::take(&mut self.items),
            labels: self.labels.len() as u32,
        };
        // Each instruction was checked as it was parsed; what remains are
        // whole-function limits (code size, constants, call sites).
        let (function, assembled) = ir::assemble(&body, define.name, define.signature.clone(), asm)
            .map_err(|error| mismatch(define.frame.1, format!("cannot assemble: {error:?}")))?;
        if function.frame_size != define.frame.0 {
            return Err(mismatch(
                define.frame.1,
                format!(
                    "the code and signature use {} slots, not frame({})",
                    function.frame_size, define.frame.0
                ),
            ));
        }
        Ok((function, assembled))
    }

    fn label(&mut self, name: &'a str, span: Span) -> u32 {
        let next = self.labels.len() as u32;
        self.labels.entry(name).or_insert((next, None, span)).0
    }

    fn push(&mut self, insn: Insn, span: Span) {
        self.items.push(Item::new(insn, span));
    }

    /// `label %name`.
    fn target(&mut self) -> Result<u32> {
        self.parser.keyword("label")?;
        match self.parser.peek().tok {
            Tok::Local(name) => {
                let span = self.parser.next().span;
                Ok(self.label(name, span))
            }
            _ => Err(self.parser.expected("a label (`%name`)")),
        }
    }

    /// A label definition or an instruction.
    fn item(&mut self) -> Result<()> {
        let first = self.parser.peek();
        if let Tok::Word(name) = first.tok
            && self.parser.tokens[self.parser.at + 1].tok == Tok::Punct(b':')
        {
            self.parser.next();
            self.parser.next();
            if name.starts_with('r')
                && name[1..].bytes().all(|b| b.is_ascii_digit())
                && name.len() > 1
            {
                return Err(syntax(first.span, "a label cannot be named like a slot"));
            }
            let label = self.label(name, first.span);
            let entry = self.labels.get_mut(name).expect("just made");
            if entry.1.is_some() {
                return Err(mismatch(
                    first.span,
                    format!("label `{name}` is defined twice"),
                ));
            }
            entry.1 = Some(first.span);
            self.push(Insn::Label(label), Span::default());
            return Ok(());
        }
        let dest = match first.tok {
            Tok::Slot(slot) => {
                self.parser.next();
                self.parser.punct(b'=')?;
                Some(slot)
            }
            _ => None,
        };
        let (word, word_span) = self.parser.word("an instruction")?;
        let op = *self
            .mnemonics
            .get(word)
            .ok_or_else(|| syntax(word_span, format!("unknown instruction `{word}`")))?;
        self.instruction(op, dest, first.span.start, word_span)
    }

    /// The instruction `op` after its mnemonic. Its span starts at `start`:
    /// the destination, if written.
    fn instruction(
        &mut self,
        op: Op,
        dest: Option<u32>,
        start: u32,
        word_span: Span,
    ) -> Result<()> {
        let info = op.info();
        let name = mnemonic(op);
        let no_dest = |dest: Option<u32>| match dest {
            Some(_) => Err(syntax(word_span, format!("`{name}` writes no slot"))),
            None => Ok(()),
        };
        let need_dest = |dest: Option<u32>| {
            dest.ok_or_else(|| {
                syntax(
                    word_span,
                    format!("`{name}` needs a destination (`%rN = {name} …`)"),
                )
            })
        };
        let unencodable =
            |e: EncodeError| mismatch(word_span, format!("cannot encode `{name}`: {e:?}"));
        let no_long_form = || {
            mismatch(
                word_span,
                format!("`{name}` with these operands has no far form, so it cannot be assembled"),
            )
        };
        match info.sem {
            Sem::LoadImm | Sem::LoadConst => {
                let dst = need_dest(dest)?;
                let (ty, _) = self.parser.value_type()?;
                let (value, value_span) = match (ty, self.parser.peek().tok) {
                    (ValueType::Bool, Tok::Word(w @ ("true" | "false"))) => {
                        let span = self.parser.next().span;
                        (u64::from(w == "true"), span)
                    }
                    (ValueType::Bool, _) => return Err(self.parser.expected("`true` or `false`")),
                    (ValueType::I64, _) => {
                        let (value, span) = self.parser.int()?;
                        let value = i64::try_from(value)
                            .map_err(|_| mismatch(span, "integer does not fit `i64`"))?;
                        (value as u64, span)
                    }
                };
                // The text names the form the assembler will choose.
                let chosen = if (i16::MIN as i64..=i16::MAX as i64).contains(&(value as i64)) {
                    Op::Ldi
                } else {
                    let next = self.constants.len();
                    let index = *self.constants.entry(value).or_insert(next);
                    if index <= u16::MAX as usize {
                        Op::Ldk
                    } else {
                        Op::LdkX
                    }
                };
                if chosen != op {
                    return Err(mismatch(
                        value_span,
                        format!("this value loads with `{}`, not `{name}`", mnemonic(chosen)),
                    ));
                }
                let span = self.span_from(start);
                self.push(Insn::Load { dst, value, ty }, span);
                Ok(())
            }
            Sem::Call(_) => self.call(op, dest, start, word_span),
            Sem::Return => {
                no_dest(dest)?;
                let span = self.span_from(start);
                self.push(Insn::Ret, span);
                Ok(())
            }
            Sem::Jump if op == Op::J => {
                no_dest(dest)?;
                let target = self.target()?;
                let span = self.span_from(start);
                let branch = Branch::Jump;
                self.push(Insn::Branch { branch, target }, span);
                Ok(())
            }
            Sem::Branch { cond, imm: false } => {
                no_dest(dest)?;
                let (a, _) = self.parser.slot()?;
                self.parser.punct(b',')?;
                let (b, _) = self.parser.slot()?;
                self.parser.punct(b',')?;
                let target = self.target()?;
                if cond.complement().is_none() {
                    return Err(no_long_form());
                }
                let span = self.span_from(start);
                let branch = Branch::Compare { cond, a, b };
                self.push(Insn::Branch { branch, target }, span);
                Ok(())
            }
            Sem::Branch { cond, imm: true } => {
                no_dest(dest)?;
                let (a, _) = self.parser.slot()?;
                self.parser.punct(b',')?;
                let (imm, imm_span) = self.parser.int()?;
                let imm =
                    i64::try_from(imm).map_err(|_| mismatch(imm_span, "immediate out of range"))?;
                self.parser.punct(b',')?;
                let target = self.target()?;
                if imm_complement(cond, imm).is_none() {
                    return Err(no_long_form());
                }
                let span = self.span_from(start);
                let branch = Branch::CompareImm { cond, a, imm };
                self.push(Insn::Branch { branch, target }, span);
                Ok(())
            }
            Sem::TestBranch(test) => {
                no_dest(dest)?;
                let (a, _) = self.parser.slot()?;
                self.parser.punct(b',')?;
                let target = self.target()?;
                let span = self.span_from(start);
                let branch = Branch::Test { test, a };
                self.push(Insn::Branch { branch, target }, span);
                Ok(())
            }
            Sem::Jump | Sem::Wide => Err(syntax(
                word_span,
                format!(
                    "`{name}` is an encoding the assembler chooses; write the logical instruction"
                ),
            )),
            _ => {
                let roles = [
                    info.roles[0],
                    info.roles[1],
                    info.roles[2],
                    info.format.ext_role(),
                ];
                let writes = roles[0] == Role::Dst;
                let mut values = [0i64; 4];
                if writes {
                    values[0] = need_dest(dest)? as i64;
                } else {
                    no_dest(dest)?;
                }
                let mut first = true;
                for i in usize::from(writes)..4 {
                    let role = roles[i];
                    if role == Role::None {
                        continue;
                    }
                    if !first {
                        self.parser.punct(b',')?;
                    }
                    first = false;
                    values[i] = match role {
                        Role::Dst | Role::Src | Role::SrcDst => self.parser.slot()?.0 as i64,
                        Role::Imm8
                        | Role::Shift8
                        | Role::Imm16
                        | Role::Index8
                        | Role::Index16
                        | Role::Offset8
                        | Role::Offset32
                        | Role::Width8 => {
                            let (value, span) = self.parser.int()?;
                            i64::try_from(value)
                                .map_err(|_| mismatch(span, "operand out of range"))?
                        }
                        _ => {
                            return Err(syntax(
                                word_span,
                                format!("`{name}` cannot be assembled from text yet"),
                            ));
                        }
                    };
                }
                let [a, b, c, ext] = values;
                let operands = Operands { a, b, c, ext };
                self.words.clear();
                encode(op, operands, self.words).map_err(unencodable)?;
                let span = self.span_from(start);
                self.push(Insn::Plain { op, operands }, span);
                Ok(())
            }
        }
    }

    fn call(&mut self, op: Op, dest: Option<u32>, start: u32, word_span: Span) -> Result<()> {
        let (ret, ret_span) = self.parser.ty(true)?;
        let (name, name_span) = self.parser.global()?;
        let (target, signature) = if op == Op::CallScript {
            let &id =
                self.headers.functions.get(name).ok_or_else(|| {
                    mismatch(name_span, format!("no function `@{name}` is defined"))
                })?;
            (
                CallTarget::Script(id),
                &self.headers.defines[id.index()].signature,
            )
        } else {
            let &index = self.headers.host_index.get(name).ok_or_else(|| {
                mismatch(name_span, format!("no host function `@{name}` is declared"))
            })?;
            (
                CallTarget::Host(index),
                &self.headers.hosts[index as usize].0.signature,
            )
        };
        if ret != signature.results.first().copied() {
            return Err(mismatch(
                ret_span,
                format!("`@{name}` does not return this type"),
            ));
        }
        self.parser.punct(b'(')?;
        let mut args: Vec<(ValueType, u32, Span)> = Vec::new();
        if !self.parser.eat_punct(b')') {
            loop {
                let (ty, _) = self.parser.value_type()?;
                let (slot, span) = self.parser.slot()?;
                args.push((ty, slot, span));
                if self.parser.eat_punct(b')') {
                    break;
                }
                self.parser.punct(b',')?;
            }
        }
        if args.len() != signature.params.len()
            || args
                .iter()
                .zip(&signature.params)
                .any(|(&(ty, _, _), &param)| ty != param)
        {
            return Err(mismatch(
                name_span,
                format!("the arguments do not match `@{name}`'s parameters"),
            ));
        }
        let first = args.first().map_or(0, |&(_, slot, _)| slot);
        for (i, &(_, slot, span)) in args.iter().enumerate() {
            if slot != first + i as u32 {
                return Err(mismatch(span, "call arguments must be consecutive slots"));
            }
        }
        let result = match (ret, dest) {
            (Some(_), Some(slot)) => SlotRange {
                first: slot,
                count: 1,
            },
            (None, None) => SlotRange { first: 0, count: 0 },
            (Some(_), None) => {
                return Err(mismatch(
                    word_span,
                    format!("`@{name}` returns a value: write `%rN = {}`", mnemonic(op)),
                ));
            }
            (None, Some(_)) => {
                return Err(mismatch(word_span, format!("`@{name}` returns nothing")));
            }
        };
        let site = CallSite {
            target,
            args: SlotRange {
                first,
                count: args.len() as u32,
            },
            result,
        };
        let span = self.span_from(start);
        self.push(Insn::Call(site), span);
        Ok(())
    }

    /// From `start` to the end of the last token consumed.
    fn span_from(&self, start: u32) -> Span {
        Span::new(start, self.parser.tokens[self.parser.at - 1].span.end)
    }
}
