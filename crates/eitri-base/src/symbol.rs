//! Spelling interning. A [`Symbol`] is the identity of a spelling, not of a
//! binding: two shadowed `x` share one symbol and get different binding ids.
//!
//! Unlike a process-wide interner, an [`Interner`] belongs to the caller's
//! context and is passed explicitly, with no locks or leaked memory. It is
//! reset between compilations, keeping its allocations, so steady-state
//! interning allocates nothing. Keywords and predeclared names are
//! predefined at the same indexes in every interner, so `kw::WHILE` is a
//! constant and keyword recognition is one range check after the lookup the
//! lexer performs anyway.
//!
//! Costs: `intern` hashes the spelling once (Fx, one multiply per 8 bytes),
//! then probes an open-addressing table with linear probing at load ≤ 1/2,
//! comparing a stored 32-bit hash tag before the bytes. Growth rehashes in
//! O(symbols). `reset` is O(symbols added since the last reset). Retained
//! memory is the high-water mark of one compilation: spelling bytes, 12
//! bytes per symbol, and 4 bytes per table slot.

use crate::fx::fx_hash;
use crate::idx::{CapacityError, IndexVec};

crate::newtype_index! {
    /// An interned spelling. Equality is integer equality. Not meaningful
    /// across interners, except for the predefined [`kw`] and [`sym`] values.
    pub struct Symbol;
}

macro_rules! symbols {
    (
        keywords { $($kw:ident: $kw_text:literal,)* }
        types { $($ty:ident: $ty_text:literal,)* }
        names { $($nm:ident: $nm_text:literal,)* }
    ) => {
        #[allow(non_camel_case_types, clippy::upper_case_acronyms)]
        #[repr(u32)]
        enum Predefined { $($kw,)* $($ty,)* $($nm,)* }

        /// Keywords: never identifiers.
        pub mod kw {
            use super::{Predefined, Symbol};
            $(pub const $kw: Symbol = Symbol::from_u32(Predefined::$kw as u32);)*
        }

        /// Predeclared type names, then contextual and well-known names.
        pub mod sym {
            use super::{Predefined, Symbol};
            $(pub const $ty: Symbol = Symbol::from_u32(Predefined::$ty as u32);)*
            $(pub const $nm: Symbol = Symbol::from_u32(Predefined::$nm as u32);)*
        }

        const PREDEFINED: &[&str] = &[$($kw_text,)* $($ty_text,)* $($nm_text,)*];
        const KEYWORDS_END: u32 = [$($kw_text),*].len() as u32;
        const TYPES_END: u32 = KEYWORDS_END + [$($ty_text),*].len() as u32;
    };
}

// The lexical structure chapter is the source of these lists.
symbols! {
    keywords {
        ASYNC: "async", AWAIT: "await", BREAK: "break", CLASS: "class",
        CONTINUE: "continue", DEFER: "defer", ELSE: "else", ENUM: "enum",
        EXTEND: "extend", FALSE: "false", FN: "fn", FOR: "for", GUARD: "guard",
        IF: "if", IMPORT: "import", IN: "in", INTERFACE: "interface", LET: "let",
        MATCH: "match", MUTATING: "mutating", NULL: "null", PUB: "pub",
        RETURN: "return", SELF_LOWER: "self", SPAWN: "spawn", STATIC: "static",
        STRUCT: "struct", TRUE: "true", TRY: "try", TYPE: "type", VAR: "var",
        WHILE: "while", YIELD: "yield",
    }
    types {
        I8: "i8", I64: "i64", U8: "u8", U64: "u64", F32: "f32", F64: "f64",
        BOOL: "bool", CHAR: "char", STRING: "string", BYTES: "bytes",
        VOID: "void", DYN: "dyn",
    }
    names {
        FROM: "from", MAP: "map", TASK: "task", WEAK: "weak", CHAN: "chan",
        ANY: "any", SELF_UPPER: "Self",
    }
}

impl Symbol {
    /// A keyword, including the reserved `yield`.
    #[inline]
    pub const fn is_keyword(self) -> bool {
        self.as_u32() < KEYWORDS_END
    }

    /// A predeclared type name, which can't be used as a name.
    #[inline]
    pub const fn is_type_name(self) -> bool {
        KEYWORDS_END <= self.as_u32() && self.as_u32() < TYPES_END
    }

    /// The spelling of a predefined symbol, without an interner.
    pub fn predefined_str(self) -> Option<&'static str> {
        PREDEFINED.get(self.index_usize()).copied()
    }

    #[inline]
    const fn index_usize(self) -> usize {
        self.as_u32() as usize
    }
}

/// Where a spelling lives in `bytes`, and the top half of its hash.
#[derive(Clone, Copy)]
struct Entry {
    start: u32,
    len: u32,
    tag: u32,
}

/// A context-owned spelling table. See the module documentation.
pub struct Interner {
    bytes: String,
    entries: IndexVec<Symbol, Entry>,
    /// Open addressing; `0` is empty, otherwise symbol index + 1. The length
    /// is a power of two. Invariant: each symbol's probe path crosses only
    /// slots of lower-numbered symbols, because symbols are inserted (and
    /// rehashed) in index order and removed only from the top.
    table: Vec<u32>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    const INITIAL_SLOTS: usize = 256;

    pub fn new() -> Self {
        let mut interner = Self {
            bytes: String::with_capacity(1024),
            entries: IndexVec::with_capacity(Self::INITIAL_SLOTS / 2),
            table: vec![0; Self::INITIAL_SLOTS],
        };
        for (index, text) in PREDEFINED.iter().enumerate() {
            let symbol = interner.intern(text);
            debug_assert_eq!(symbol.index_usize(), index, "duplicate predefined `{text}`");
        }
        interner
    }

    /// The symbol for `text`, adding it if new.
    ///
    /// # Panics
    /// When the table exceeds `u32` offsets; see [`Self::try_intern`].
    #[inline]
    pub fn intern(&mut self, text: &str) -> Symbol {
        self.try_intern(text).expect("interner overflow")
    }

    pub fn try_intern(&mut self, text: &str) -> Result<Symbol, CapacityError> {
        let hash = fx_hash(text.as_bytes());
        let slot = match self.probe(hash, text) {
            Ok(symbol) => return Ok(symbol),
            Err(slot) => slot,
        };

        let start = u32::try_from(self.bytes.len()).map_err(|_| CapacityError)?;
        let len = u32::try_from(text.len()).map_err(|_| CapacityError)?;
        start.checked_add(len).ok_or(CapacityError)?;
        let symbol = self.entries.try_push(Entry {
            start,
            len,
            tag: (hash >> 32) as u32,
        })?;
        self.bytes.push_str(text);
        self.table[slot] = symbol.as_u32() + 1;
        if self.entries.len() * 2 > self.table.len() {
            self.grow();
        }
        Ok(symbol)
    }

    /// The symbol for `text` if it was interned, without adding it.
    pub fn get(&self, text: &str) -> Option<Symbol> {
        self.probe(fx_hash(text.as_bytes()), text).ok()
    }

    /// The spelling of a symbol from this interner.
    ///
    /// # Panics
    /// If `symbol` came from another interner and is out of range here.
    #[inline]
    pub fn resolve(&self, symbol: Symbol) -> &str {
        let entry = self.entries[symbol];
        &self.bytes[entry.start as usize..(entry.start + entry.len) as usize]
    }

    /// Symbols currently interned, predefined ones included.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Never true: predefined symbols are always present.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forget every symbol added since construction, keeping allocations.
    /// Predefined symbols remain.
    pub fn reset(&mut self) {
        let keep = PREDEFINED.len();
        let added = self.entries.len() - keep;
        if added > self.table.len() / 8 {
            // Many removals: clearing and reinserting the few predefined
            // symbols is cheaper than probing for each removed one.
            self.table.fill(0);
            self.entries.truncate(Symbol::from_u32(keep as u32));
            self.bytes.truncate(self.end_of(keep));
            self.rehash();
            return;
        }
        // Remove from the top, so every remaining probe path stays intact.
        for index in (keep..self.entries.len()).rev() {
            let symbol = Symbol::from_u32(index as u32);
            let hash = fx_hash(self.resolve(symbol).as_bytes());
            let mask = self.table.len() - 1;
            let mut slot = hash as usize & mask;
            while self.table[slot] != symbol.as_u32() + 1 {
                slot = (slot + 1) & mask;
            }
            self.table[slot] = 0;
        }
        self.entries.truncate(Symbol::from_u32(keep as u32));
        self.bytes.truncate(self.end_of(keep));
    }

    /// Heap bytes retained, for scratch high-water accounting.
    pub fn heap_bytes(&self) -> usize {
        self.bytes.capacity()
            + self.entries.capacity() * size_of::<Entry>()
            + self.table.capacity() * size_of::<u32>()
    }

    /// End of the spelling bytes of the first `count` symbols.
    fn end_of(&self, count: usize) -> usize {
        match count.checked_sub(1) {
            Some(last) => {
                let entry = self.entries.as_slice()[last];
                (entry.start + entry.len) as usize
            }
            None => 0,
        }
    }

    /// `Ok` with the symbol for `text`, or `Err` with the empty slot where
    /// it belongs.
    #[inline]
    fn probe(&self, hash: u64, text: &str) -> Result<Symbol, usize> {
        let mask = self.table.len() - 1;
        let tag = (hash >> 32) as u32;
        let mut slot = hash as usize & mask;
        loop {
            let Some(index) = self.table[slot].checked_sub(1) else {
                return Err(slot);
            };
            let symbol = Symbol::from_u32(index);
            let entry = self.entries[symbol];
            if entry.tag == tag && self.resolve(symbol) == text {
                return Ok(symbol);
            }
            slot = (slot + 1) & mask;
        }
    }

    #[cold]
    fn grow(&mut self) {
        let slots = self.table.len() * 2;
        self.table.clear();
        self.table.resize(slots, 0);
        self.rehash();
    }

    /// Insert every symbol into an empty table, in index order.
    fn rehash(&mut self) {
        let mask = self.table.len() - 1;
        for symbol in self.entries.indices() {
            let hash = fx_hash(self.resolve(symbol).as_bytes());
            let mut slot = hash as usize & mask;
            while self.table[slot] != 0 {
                slot = (slot + 1) & mask;
            }
            self.table[slot] = symbol.as_u32() + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_symbols_are_constants_in_every_interner() {
        let mut a = Interner::new();
        let b = Interner::new();
        assert_eq!(a.intern("while"), kw::WHILE);
        assert_eq!(b.get("i64"), Some(sym::I64));
        for (index, text) in PREDEFINED.iter().enumerate() {
            assert_eq!(a.get(text), Some(Symbol::from_u32(index as u32)), "{text}");
            assert_eq!(a.resolve(Symbol::from_u32(index as u32)), *text);
        }
        assert_eq!(sym::SELF_UPPER.predefined_str(), Some("Self"));
    }

    #[test]
    fn classes() {
        assert!(kw::YIELD.is_keyword() && kw::SELF_LOWER.is_keyword());
        assert!(sym::VOID.is_type_name() && !sym::VOID.is_keyword());
        assert!(!sym::MAP.is_keyword() && !sym::MAP.is_type_name());
        let mut interner = Interner::new();
        let name = interner.intern("frobnicate");
        assert!(!name.is_keyword() && !name.is_type_name());
    }

    #[test]
    fn interning_is_stable_and_collision_safe() {
        let mut interner = Interner::new();
        let base = interner.len();
        let names: Vec<String> = (0..5000).map(|i| format!("name_{i}")).collect();
        let symbols: Vec<Symbol> = names.iter().map(|n| interner.intern(n)).collect();
        // Growth rehashed several times; every symbol still resolves.
        for (name, &symbol) in names.iter().zip(&symbols) {
            assert_eq!(interner.intern(name), symbol);
            assert_eq!(interner.resolve(symbol), name);
        }
        assert_eq!(interner.len(), base + 5000);
        assert_eq!(interner.intern(""), interner.intern(""));
        assert_ne!(interner.intern("a"), interner.intern("a\0"));
        assert_eq!(interner.get("missing"), None);
    }

    #[test]
    fn reset_keeps_predefined_and_reuses_storage() {
        let mut interner = Interner::new();
        let base = interner.len();
        for count in [3, 40, 2000, 1] {
            let names: Vec<String> = (0..count).map(|i| format!("v{i}")).collect();
            for name in &names {
                interner.intern(name);
            }
            interner.intern("while");
            interner.reset();
            assert_eq!(interner.len(), base);
            assert_eq!(interner.get("v0"), None);
            assert_eq!(interner.get("while"), Some(kw::WHILE));
            for (index, text) in PREDEFINED.iter().enumerate() {
                assert_eq!(interner.get(text), Some(Symbol::from_u32(index as u32)));
            }
            // A fresh compilation numbers its spellings from the same start.
            assert_eq!(interner.intern("first").index_usize(), base);
            interner.reset();
        }
        let retained = interner.heap_bytes();
        for i in 0..1000 {
            interner.intern(&format!("v{i}"));
        }
        interner.reset();
        assert_eq!(
            interner.heap_bytes(),
            retained,
            "steady state must not grow"
        );
    }

    #[test]
    fn interners_are_independent() {
        let mut a = Interner::new();
        let mut b = Interner::new();
        let x = a.intern("x");
        let y = b.intern("y");
        assert_eq!(x, y, "same index, different interners");
        assert_eq!((a.resolve(x), b.resolve(y)), ("x", "y"));
        assert_eq!(a.get("y"), None);
    }
}
