//! Source identity and byte spans, shared by tokens, syntax, and diagnostics.

use std::ops::Range;

crate::newtype_index! {
    /// A source registered for one run: the submitted script first, then
    /// imported modules.
    pub struct SourceId;
}

/// Half-open UTF-8 byte range into one source. Offsets are `u32` because the
/// compiler rejects sources of 4 GiB or more (`E0002`), which halves the
/// size of every span stored in tokens and syntax nodes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

const _: () = assert!(size_of::<Span>() == 8);

impl Span {
    #[inline]
    pub const fn new(start: u32, end: u32) -> Self {
        debug_assert!(start <= end);
        Self { start, end }
    }

    /// An empty span at `offset`, e.g. for "expected `)` here".
    #[inline]
    pub const fn empty(offset: u32) -> Self {
        Self::new(offset, offset)
    }

    /// The smallest span covering both.
    #[inline]
    pub const fn to(self, other: Span) -> Span {
        let start = if self.start < other.start {
            self.start
        } else {
            other.start
        };
        let end = if self.end > other.end {
            self.end
        } else {
            other.end
        };
        Span::new(start, end)
    }

    /// An empty span at the end.
    #[inline]
    pub const fn shrink_to_end(self) -> Span {
        Span::empty(self.end)
    }

    #[inline]
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    #[inline]
    pub const fn contains(self, offset: u32) -> bool {
        self.start <= offset && offset < self.end
    }

    #[inline]
    pub const fn range(self) -> Range<usize> {
        self.start as usize..self.end as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_combine() {
        let a = Span::new(4, 6);
        let b = Span::new(1, 2);
        assert_eq!((a.to(b), b.to(a)), (Span::new(1, 6), Span::new(1, 6)));
        assert_eq!(a.shrink_to_end(), Span::empty(6));
        assert!(a.contains(5) && !a.contains(6));
        assert_eq!((a.len(), a.range()), (2, 4..6));
    }
}
