//! Token kinds and the compact token buffer.

use eitri_base::{Idx, Span, Symbol, kw};

eitri_base::newtype_index! {
    /// A token in one compilation's token buffer.
    pub struct TokenId;
}

macro_rules! token_kinds {
    (
        names { $($name:ident: $name_text:literal,)* }
        keywords { $($kw:ident: $kw_sym:ident = $kw_text:literal,)* }
        punct { $($punct:ident: $punct_text:literal,)* }
    ) => {
        /// What a token is. Keywords come in the same order as the
        /// predefined keyword symbols, so mapping one to the other is an
        /// array lookup.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum TokenKind {
            $($name,)*
            $($kw,)*
            $($punct,)*
        }

        /// Keyword kinds, indexed by keyword symbol.
        const KEYWORDS: &[(Symbol, TokenKind)] = &[$((kw::$kw_sym, TokenKind::$kw),)*];

        impl TokenKind {
            /// How diagnostics name the token: a quoted spelling, or a
            /// description for tokens with variable spelling.
            pub fn describe(self) -> &'static str {
                match self {
                    $(TokenKind::$name => $name_text,)*
                    $(TokenKind::$kw => concat!("`", $kw_text, "`"),)*
                    $(TokenKind::$punct => $punct_text,)*
                }
            }
        }
    };
}

token_kinds! {
    names {
        Ident: "identifier",
        Wildcard: "`_`",
        Attribute: "attribute",
        Int: "integer literal",
        Float: "float literal",
        Str: "string literal",
        Char: "char literal",
        Newline: "line break",
        Eof: "end of input",
    }
    keywords {
        Async: ASYNC = "async", Await: AWAIT = "await", Break: BREAK = "break",
        Class: CLASS = "class", Continue: CONTINUE = "continue", Defer: DEFER = "defer",
        Else: ELSE = "else", Enum: ENUM = "enum", Extend: EXTEND = "extend",
        False: FALSE = "false", Fn: FN = "fn", For: FOR = "for", Guard: GUARD = "guard",
        If: IF = "if", Import: IMPORT = "import", In: IN = "in",
        Interface: INTERFACE = "interface", Let: LET = "let", Match: MATCH = "match",
        Mutating: MUTATING = "mutating", Null: NULL = "null", Pub: PUB = "pub",
        Return: RETURN = "return", SelfLower: SELF_LOWER = "self", Spawn: SPAWN = "spawn",
        Static: STATIC = "static", Struct: STRUCT = "struct", True: TRUE = "true",
        Try: TRY = "try", Type: TYPE = "type", Var: VAR = "var", While: WHILE = "while",
        Yield: YIELD = "yield",
    }
    punct {
        LParen: "`(`", RParen: "`)`", LBracket: "`[`", RBracket: "`]`",
        LBrace: "`{`", RBrace: "`}`", Comma: "`,`", Semi: "`;`", Colon: "`:`",
        Dot: "`.`", QuestionDot: "`?.`", QuestionBracket: "`?[`",
        QuestionQuestion: "`??`", Question: "`?`", TryQuestion: "`try?`",
        FatArrow: "`=>`", Arrow: "`->`", DotDot: "`..`", DotDotEq: "`..=`",
        EqEq: "`==`", NotEq: "`!=`", Lt: "`<`", LtEq: "`<=`", Gt: "`>`",
        GtEq: "`>=`", AndAnd: "`&&`", OrOr: "`||`", Bang: "`!`", Tilde: "`~`",
        Plus: "`+`", Minus: "`-`", Star: "`*`", Slash: "`/`", Percent: "`%`",
        WrapAdd: "`&+`", WrapSub: "`&-`", WrapMul: "`&*`", Amp: "`&`",
        Pipe: "`|`", Caret: "`^`", Shl: "`<<`", Eq: "`=`", PlusEq: "`+=`",
        MinusEq: "`-=`", StarEq: "`*=`", SlashEq: "`/=`", PercentEq: "`%=`",
        WrapAddEq: "`&+=`", WrapSubEq: "`&-=`", WrapMulEq: "`&*=`",
        AmpEq: "`&=`", PipeEq: "`|=`", CaretEq: "`^=`", ShlEq: "`<<=`",
    }
}

impl TokenKind {
    pub fn is_keyword(self) -> bool {
        (TokenKind::Async as u8..=TokenKind::Yield as u8).contains(&(self as u8))
    }

    /// The keyword kind for a keyword symbol.
    pub fn keyword(symbol: Symbol) -> Option<TokenKind> {
        symbol
            .is_keyword()
            .then(|| KEYWORDS[symbol.as_u32() as usize].1)
    }

    /// A token after which a line break does not end the statement.
    pub fn continues_line(self) -> bool {
        use TokenKind::*;
        matches!(
            self,
            Comma
                | Eq
                | FatArrow
                | Arrow
                | Dot
                | QuestionDot
                | Colon
                | OrOr
                | AndAnd
                | QuestionQuestion
                | DotDot
                | DotDotEq
                | EqEq
                | NotEq
                | LtEq
                | GtEq
                | Plus
                | Minus
                | Star
                | Slash
                | Percent
                | WrapAdd
                | WrapSub
                | WrapMul
                | Pipe
                | Caret
                | Amp
                | Shl
                | PlusEq
                | MinusEq
                | StarEq
                | SlashEq
                | PercentEq
                | WrapAddEq
                | WrapSubEq
                | WrapMulEq
                | AmpEq
                | PipeEq
                | CaretEq
                | ShlEq
                | Try
                | TryQuestion
                | Await
                | Spawn
        )
    }

    pub fn is_open_delimiter(self) -> bool {
        matches!(
            self,
            TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::QuestionBracket
                | TokenKind::LBrace
        )
    }

    /// The closing kind for an opening delimiter.
    pub fn closer(self) -> Option<TokenKind> {
        match self {
            TokenKind::LParen => Some(TokenKind::RParen),
            TokenKind::LBracket | TokenKind::QuestionBracket => Some(TokenKind::RBracket),
            TokenKind::LBrace => Some(TokenKind::RBrace),
            _ => None,
        }
    }
}

/// Payload of a delimiter whose partner is missing.
const UNMATCHED: u32 = u32::MAX;

/// One compilation's tokens, stored as parallel arrays: 13 bytes per token
/// (kind, span, payload). The payload is the [`Symbol`] of an identifier or
/// attribute name, or the index of a delimiter's partner, so the parser can
/// skip a bracketed group in O(1). Lives in a [`LexScratch`](crate::LexScratch)
/// and is refilled by each compilation.
#[derive(Default)]
pub struct Tokens {
    // Plain vectors, not `IndexVec`s: `push` checks capacity and the id once
    // for all three columns instead of once per column (lexer step 1).
    kinds: Vec<TokenKind>,
    spans: Vec<Span>,
    payloads: Vec<u32>,
    /// The smallest of the three columns' capacities: `push` may write
    /// below it without growing.
    capacity: usize,
}

impl Tokens {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn clear(&mut self) {
        self.kinds.clear();
        self.spans.clear();
        self.payloads.clear();
    }

    pub(crate) fn reserve(&mut self, additional: usize) {
        self.kinds.reserve(additional);
        self.spans.reserve(additional);
        self.payloads.reserve(additional);
        self.capacity = self
            .kinds
            .capacity()
            .min(self.spans.capacity())
            .min(self.payloads.capacity());
    }

    /// Append a token. The columns always have equal lengths; growing them
    /// together keeps their capacities equal, so the common path is one
    /// predictable comparison before three stores.
    #[inline]
    pub(crate) fn push(&mut self, kind: TokenKind, span: Span, payload: u32) -> TokenId {
        let index = self.kinds.len();
        if index == self.capacity {
            self.grow();
        }
        // SAFETY: the columns always have equal lengths (`index`), and
        // `capacity` is at most each column's capacity and greater than
        // `index` after the check above, so slot `index` of every column is
        // allocated and unused. Each slot is written before `set_len`
        // exposes it; the element types are `Copy`, so nothing is leaked or
        // dropped. Cost removed: the three per-column capacity checks and
        // length updates `Vec::push` repeats after the shared check (lexer
        // step 5 measurement in the research ledger, E2).
        unsafe {
            self.kinds.as_mut_ptr().add(index).write(kind);
            self.spans.as_mut_ptr().add(index).write(span);
            self.payloads.as_mut_ptr().add(index).write(payload);
            self.kinds.set_len(index + 1);
            self.spans.set_len(index + 1);
            self.payloads.set_len(index + 1);
        }
        // Token count never exceeds source bytes + 1, and sources are below
        // 4 GiB, so this cannot reach the overflow branch.
        TokenId::from_u32(index as u32)
    }

    #[cold]
    #[inline(never)]
    fn grow(&mut self) {
        let additional = self.kinds.capacity().max(64);
        self.reserve(additional);
    }

    #[inline]
    pub(crate) fn link(&mut self, open: TokenId, close: TokenId) {
        self.payloads[open.index()] = close.as_u32();
        self.payloads[close.index()] = open.as_u32();
    }

    pub(crate) fn mark_unmatched(&mut self, token: TokenId) {
        self.payloads[token.index()] = UNMATCHED;
    }

    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    #[inline]
    pub fn kind(&self, token: TokenId) -> TokenKind {
        self.kinds[token.index()]
    }

    #[inline]
    pub fn span(&self, token: TokenId) -> Span {
        self.spans[token.index()]
    }

    pub fn kinds(&self) -> &[TokenKind] {
        &self.kinds
    }

    pub fn last_kind(&self) -> Option<TokenKind> {
        self.kinds.last().copied()
    }

    /// The spelling of an identifier, keyword, or attribute name.
    ///
    /// # Panics
    /// In debug builds, for other tokens.
    #[inline]
    pub fn symbol(&self, token: TokenId) -> Symbol {
        let kind = self.kind(token);
        debug_assert!(matches!(kind, TokenKind::Ident | TokenKind::Attribute) || kind.is_keyword());
        Symbol::from_u32(self.payloads[token.index()])
    }

    /// The partner of a delimiter, if it has one.
    #[inline]
    pub fn matching(&self, token: TokenId) -> Option<TokenId> {
        let payload = self.payloads[token.index()];
        let delimiter = self.kind(token).is_open_delimiter()
            || matches!(
                self.kind(token),
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace
            );
        (delimiter && payload != UNMATCHED).then(|| TokenId::from_u32(payload))
    }

    /// Heap bytes retained, for scratch high-water accounting.
    pub fn heap_bytes(&self) -> usize {
        self.kinds.capacity() * size_of::<TokenKind>()
            + self.spans.capacity() * size_of::<Span>()
            + self.payloads.capacity() * size_of::<u32>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eitri_base::Interner;

    #[test]
    fn keywords_follow_symbol_order() {
        let interner = Interner::new();
        for (index, &(symbol, kind)) in KEYWORDS.iter().enumerate() {
            assert_eq!(symbol.as_u32() as usize, index);
            assert_eq!(TokenKind::keyword(symbol), Some(kind));
            assert_eq!(kind.describe(), format!("`{}`", interner.resolve(symbol)));
        }
        assert_eq!(TokenKind::keyword(interner.get("i64").unwrap()), None);
        assert_eq!(size_of::<TokenKind>(), 1);
    }
}
