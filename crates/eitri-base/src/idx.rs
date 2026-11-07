//! Typed dense indexes, the vectors they index, and contiguous id ranges.

use std::fmt;
use std::iter::FusedIterator;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::ops::{Index, IndexMut};

/// A dense index type declared with [`newtype_index!`](crate::newtype_index).
pub trait Idx: Copy + Eq + Ord + std::hash::Hash + fmt::Debug + 'static {
    /// # Panics
    /// If `index` exceeds [`MAX_INDEX`].
    fn new(index: usize) -> Self;
    fn index(self) -> usize;
}

/// The largest index an [`Idx`] type holds; `u32::MAX` is the `Option` niche.
pub const MAX_INDEX: u32 = u32::MAX - 1;

/// Store `index + 1`. `u32::MAX` wraps to zero and panics, so no `unsafe`
/// is needed: the bound check and the niche come from the same branch.
#[doc(hidden)]
#[inline(always)]
pub const fn encode(index: u32, name: &'static str) -> NonZeroU32 {
    match NonZeroU32::new(index.wrapping_add(1)) {
        Some(raw) => raw,
        None => index_overflow(name),
    }
}

#[doc(hidden)]
#[inline(always)]
pub fn encode_usize(index: usize, name: &'static str) -> NonZeroU32 {
    match u32::try_from(index) {
        Ok(index) => encode(index, name),
        Err(_) => index_overflow(name),
    }
}

#[cold]
#[inline(never)]
#[track_caller]
const fn index_overflow(name: &'static str) -> ! {
    let _ = name;
    panic!("index overflow")
}

/// Declare a dense index type: a `NonZeroU32` newtype, so `Option<Id>` is
/// four bytes, with `const` constructors for predefined values.
///
/// ```
/// eitri_base::newtype_index! {
///     /// A lexical declaration in one body.
///     pub struct BindingId;
/// }
/// let mut names = eitri_base::IndexVec::<BindingId, &str>::new();
/// let x = names.push("x");
/// assert_eq!((names[x], format!("{x:?}")), ("x", "BindingId(0)".into()));
/// ```
#[macro_export]
macro_rules! newtype_index {
    ($($(#[$meta:meta])* $vis:vis struct $name:ident;)+) => {$(
        $(#[$meta])*
        #[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[repr(transparent)]
        $vis struct $name(::core::num::NonZeroU32);

        impl $name {
            #[inline(always)]
            pub const fn from_u32(index: u32) -> Self {
                Self($crate::encode(index, stringify!($name)))
            }

            #[inline(always)]
            pub const fn as_u32(self) -> u32 {
                self.0.get() - 1
            }
        }

        impl $crate::Idx for $name {
            #[inline(always)]
            fn new(index: usize) -> Self {
                Self($crate::__encode_usize(index, stringify!($name)))
            }

            #[inline(always)]
            fn index(self) -> usize {
                self.as_u32() as usize
            }
        }

        impl ::core::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.as_u32())
            }
        }
    )+};
}

/// An index domain ran out of indexes. Producers turn this into an
/// implementation-limit diagnostic rather than panicking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapacityError;

impl fmt::Display for CapacityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("index space exhausted")
    }
}

impl std::error::Error for CapacityError {}

/// A `Vec` indexed by a typed id, so ids from another domain don't compile.
pub struct IndexVec<I: Idx, T> {
    raw: Vec<T>,
    marker: PhantomData<fn(&I)>,
}

impl<I: Idx, T> IndexVec<I, T> {
    #[inline]
    pub const fn new() -> Self {
        Self {
            raw: Vec::new(),
            marker: PhantomData,
        }
    }

    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            raw: Vec::with_capacity(capacity),
            marker: PhantomData,
        }
    }

    /// # Panics
    /// If the vector holds more than [`MAX_INDEX`] + 1 items.
    pub fn from_raw(raw: Vec<T>) -> Self {
        assert!(raw.len() <= MAX_INDEX as usize + 1, "index overflow");
        Self {
            raw,
            marker: PhantomData,
        }
    }

    pub fn into_raw(self) -> Vec<T> {
        self.raw
    }

    /// # Panics
    /// When the domain is full; use [`Self::try_push`] where input decides.
    #[inline]
    pub fn push(&mut self, value: T) -> I {
        let id = I::new(self.raw.len());
        self.raw.push(value);
        id
    }

    #[inline]
    pub fn try_push(&mut self, value: T) -> Result<I, CapacityError> {
        if self.raw.len() > MAX_INDEX as usize {
            return Err(CapacityError);
        }
        Ok(self.push(value))
    }

    /// Push several values with consecutive ids, e.g. a struct's fields.
    pub fn push_all(&mut self, values: impl IntoIterator<Item = T>) -> IdRange<I> {
        let start = self.raw.len();
        self.raw.extend(values);
        assert!(self.raw.len() <= MAX_INDEX as usize + 1, "index overflow");
        IdRange::from_raw(start as u32, self.raw.len() as u32)
    }

    /// The id the next `push` returns.
    #[inline]
    pub fn next_index(&self) -> I {
        I::new(self.raw.len())
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Allocated slots, for retained-memory accounting.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.raw.capacity()
    }

    #[inline]
    pub fn get(&self, id: I) -> Option<&T> {
        self.raw.get(id.index())
    }

    #[inline]
    pub fn get_mut(&mut self, id: I) -> Option<&mut T> {
        self.raw.get_mut(id.index())
    }

    #[inline]
    pub fn as_slice(&self) -> &[T] {
        &self.raw
    }

    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.raw
    }

    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.raw.iter()
    }

    /// Every id currently in the vector.
    #[inline]
    pub fn indices(&self) -> IdRange<I> {
        IdRange::from_raw(0, self.raw.len() as u32)
    }

    pub fn iter_enumerated(&self) -> impl ExactSizeIterator<Item = (I, &T)> + DoubleEndedIterator {
        self.indices().iter().zip(&self.raw)
    }

    /// Drop every id at or after `next`, keeping the allocation.
    pub fn truncate(&mut self, next: I) {
        self.raw.truncate(next.index());
    }

    /// Drop everything, keeping the allocation for the next compilation.
    pub fn clear(&mut self) {
        self.raw.clear();
    }

    /// Grow with `fill` so that `id` is valid, for side tables keyed by ids
    /// from another vector.
    pub fn ensure(&mut self, id: I, fill: impl FnMut() -> T) -> &mut T {
        if self.raw.len() <= id.index() {
            self.raw.resize_with(id.index() + 1, fill);
        }
        &mut self.raw[id.index()]
    }
}

impl<I: Idx, T> Default for IndexVec<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: Idx, T: Clone> Clone for IndexVec<I, T> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            marker: PhantomData,
        }
    }
}

impl<I: Idx, T: fmt::Debug> fmt::Debug for IndexVec<I, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter_enumerated()).finish()
    }
}

impl<I: Idx, T> Index<I> for IndexVec<I, T> {
    type Output = T;

    #[inline(always)]
    fn index(&self, id: I) -> &T {
        &self.raw[id.index()]
    }
}

impl<I: Idx, T> IndexMut<I> for IndexVec<I, T> {
    #[inline(always)]
    fn index_mut(&mut self, id: I) -> &mut T {
        &mut self.raw[id.index()]
    }
}

impl<I: Idx, T> Index<IdRange<I>> for IndexVec<I, T> {
    type Output = [T];

    #[inline]
    fn index(&self, range: IdRange<I>) -> &[T] {
        &self.raw[range.indexes()]
    }
}

impl<I: Idx, T> IndexMut<IdRange<I>> for IndexVec<I, T> {
    #[inline]
    fn index_mut(&mut self, range: IdRange<I>) -> &mut [T] {
        &mut self.raw[range.indexes()]
    }
}

impl<'a, I: Idx, T> IntoIterator for &'a IndexVec<I, T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.raw.iter()
    }
}

/// A half-open run of consecutive ids, e.g. the fields of a struct pushed
/// together with [`IndexVec::push_all`]. Eight bytes; iterating allocates
/// nothing, and `vec[range]` is the matching slice.
pub struct IdRange<I> {
    start: u32,
    end: u32,
    marker: PhantomData<fn() -> I>,
}

const _: () = assert!(size_of::<IdRange<()>>() == 8);

impl<I> IdRange<I> {
    pub const EMPTY: Self = Self::from_raw(0, 0);

    /// `end` may be one past [`MAX_INDEX`], so a range can reach the last id.
    const fn from_raw(start: u32, end: u32) -> Self {
        Self {
            start,
            end,
            marker: PhantomData,
        }
    }

    #[inline]
    pub const fn len(self) -> usize {
        (self.end - self.start) as usize
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The range as plain indexes into the backing storage.
    #[inline]
    pub const fn indexes(self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }
}

impl<I: Idx> IdRange<I> {
    /// The ids from `start` up to, not including, `end`.
    ///
    /// # Panics
    /// If `end` precedes `start`.
    pub fn new(start: I, end: I) -> Self {
        assert!(start <= end, "id range ends before it starts");
        Self::from_raw(start.index() as u32, end.index() as u32)
    }

    #[inline]
    pub fn contains(self, id: I) -> bool {
        self.indexes().contains(&id.index())
    }

    #[inline]
    pub fn iter(self) -> IdIter<I> {
        IdIter {
            next: self.start,
            end: self.end,
            marker: PhantomData,
        }
    }
}

impl<I> Clone for IdRange<I> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<I> Copy for IdRange<I> {}

impl<I> PartialEq for IdRange<I> {
    fn eq(&self, other: &Self) -> bool {
        (self.start, self.end) == (other.start, other.end)
    }
}

impl<I> Eq for IdRange<I> {}

impl<I> std::hash::Hash for IdRange<I> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.start, self.end).hash(state);
    }
}

impl<I> Default for IdRange<I> {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl<I> fmt::Debug for IdRange<I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl<I: Idx> IntoIterator for IdRange<I> {
    type Item = I;
    type IntoIter = IdIter<I>;

    fn into_iter(self) -> IdIter<I> {
        self.iter()
    }
}

/// Iterator over an [`IdRange`].
#[derive(Clone)]
pub struct IdIter<I> {
    next: u32,
    end: u32,
    marker: PhantomData<fn() -> I>,
}

impl<I: Idx> Iterator for IdIter<I> {
    type Item = I;

    #[inline]
    fn next(&mut self) -> Option<I> {
        (self.next < self.end).then(|| {
            self.next += 1;
            I::new(self.next as usize - 1)
        })
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = (self.end - self.next) as usize;
        (len, Some(len))
    }
}

impl<I: Idx> DoubleEndedIterator for IdIter<I> {
    #[inline]
    fn next_back(&mut self) -> Option<I> {
        (self.next < self.end).then(|| {
            self.end -= 1;
            I::new(self.end as usize)
        })
    }
}

impl<I: Idx> ExactSizeIterator for IdIter<I> {}

impl<I: Idx> FusedIterator for IdIter<I> {}

#[cfg(test)]
mod tests {
    use super::*;

    crate::newtype_index! {
        struct TestId;
    }

    #[test]
    fn option_is_free_and_constants_work() {
        const SEVEN: TestId = TestId::from_u32(7);
        assert_eq!(size_of::<Option<TestId>>(), 4);
        assert_eq!((SEVEN.as_u32(), SEVEN.index()), (7, 7));
        assert_eq!(format!("{SEVEN:?}"), "TestId(7)");
        assert_eq!(TestId::from_u32(MAX_INDEX).as_u32(), MAX_INDEX);
        assert!(TestId::new(1) < TestId::new(2));
    }

    #[test]
    #[should_panic(expected = "index overflow")]
    fn index_past_max_panics() {
        TestId::from_u32(u32::MAX);
    }

    #[test]
    #[should_panic(expected = "index overflow")]
    fn usize_past_u32_panics() {
        TestId::new(u32::MAX as usize + 5);
    }

    #[test]
    fn index_vec_pushes_ranges_and_side_tables() {
        let mut names: IndexVec<TestId, &str> = IndexVec::new();
        let a = names.push("a");
        let fields = names.push_all(["x", "y", "z"]);
        assert_eq!(names.next_index(), TestId::new(4));
        assert_eq!((names[a], &names[fields]), ("a", &["x", "y", "z"][..]));
        assert_eq!(fields.len(), 3);
        assert!(fields.contains(TestId::new(3)) && !fields.contains(a));
        let ids: Vec<_> = fields.iter().rev().map(TestId::as_u32).collect();
        assert_eq!(ids, [3, 2, 1]);
        assert_eq!(fields.iter().len(), 3);
        assert_eq!(format!("{fields:?}"), "1..4");

        let mut kinds: IndexVec<TestId, u8> = IndexVec::new();
        *kinds.ensure(TestId::new(2), || 0) = 9;
        assert_eq!(kinds.as_slice(), [0, 0, 9]);

        names.truncate(TestId::new(1));
        assert_eq!(names.len(), 1);
        names.clear();
        assert!(names.is_empty() && names.indices().is_empty());
        assert_eq!(names.try_push("again"), Ok(TestId::new(0)));
    }
}
