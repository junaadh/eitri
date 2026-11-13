//! Variable-length lists stored back to back in one shared pool.
//!
//! A syntax node, signature, or descriptor names its children with an
//! eight-byte [`List`] instead of owning a `Vec`, so building a tree makes
//! one growing allocation per pool rather than one per node, and browsing
//! children is a slice borrow.

use std::fmt;
use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

use crate::idx::CapacityError;

/// A run of `T`s in a [`ListPool<T>`]. Only meaningful with the pool that
/// produced it; indexing another pool gives wrong items or panics, never
/// undefined behavior.
pub struct List<T> {
    start: u32,
    len: u32,
    marker: PhantomData<fn() -> T>,
}

const _: () = assert!(size_of::<List<u64>>() == 8);

impl<T> List<T> {
    pub const EMPTY: Self = Self {
        start: 0,
        len: 0,
        marker: PhantomData,
    };

    #[inline]
    pub const fn len(self) -> usize {
        self.len as usize
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    #[inline]
    const fn range(self) -> std::ops::Range<usize> {
        self.start as usize..self.start as usize + self.len as usize
    }
}

impl<T> Copy for List<T> {}

impl<T> Clone for List<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Default for List<T> {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl<T> PartialEq for List<T> {
    fn eq(&self, other: &Self) -> bool {
        (self.start, self.len) == (other.start, other.len)
    }
}

impl<T> Eq for List<T> {}

impl<T> std::hash::Hash for List<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.start, self.len).hash(state);
    }
}

impl<T> fmt::Debug for List<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "List[{}..{}]", self.start, self.start + self.len)
    }
}

/// Backing storage for many [`List`]s in one vector.
pub struct ListPool<T> {
    items: Vec<T>,
}

impl<T> Default for ListPool<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> ListPool<T> {
    pub const fn new() -> Self {
        Self { items: Vec::new() }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: Vec::with_capacity(capacity),
        }
    }

    /// Backing allocation in bytes, excluding allocator bookkeeping.
    pub fn heap_bytes(&self) -> usize {
        self.items.capacity() * size_of::<T>()
    }

    /// Reserve a total number of items using an explicit budget.
    pub fn reserve_budgeted(
        &mut self,
        total: usize,
        budget: &mut crate::MemoryBudget,
    ) -> Result<(), crate::OutOfMemory> {
        let additional = total.saturating_sub(self.items.len());
        budget.reserve_vec(&mut self.items, additional)
    }

    /// Total items across all lists, for memory accounting.
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// Forget every list, keeping the allocation for the next compilation.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Seal the items pushed since `start` as one list.
    fn seal(&mut self, start: usize) -> Result<List<T>, CapacityError> {
        let end = self.items.len();
        if end == start {
            return Ok(List::EMPTY);
        }
        match (u32::try_from(start), u32::try_from(end)) {
            (Ok(start), Ok(end)) => Ok(List {
                start,
                len: end - start,
                marker: PhantomData,
            }),
            _ => {
                self.items.truncate(start);
                Err(CapacityError)
            }
        }
    }

    /// Move the items an iterator yields into one list. An empty iterator
    /// returns [`List::EMPTY`] without touching the pool.
    pub fn try_extend(
        &mut self,
        items: impl IntoIterator<Item = T>,
    ) -> Result<List<T>, CapacityError> {
        let start = self.items.len();
        self.items.extend(items);
        self.seal(start)
    }

    /// # Panics
    /// When the pool exceeds `u32` offsets.
    pub fn extend(&mut self, items: impl IntoIterator<Item = T>) -> List<T> {
        self.try_extend(items).expect("list pool overflow")
    }

    /// Move `scratch[from..]` into one list and truncate `scratch` to `from`.
    ///
    /// For nested construction: a parser pushes each child onto one shared
    /// scratch stack (children of children land above it and are sealed
    /// first), then seals its own run. Neither the scratch stack nor the
    /// pool allocates per node once warm.
    pub fn push_drain(&mut self, scratch: &mut Vec<T>, from: usize) -> List<T> {
        self.extend(scratch.drain(from..))
    }
}

impl<T: Copy> ListPool<T> {
    /// Copy `items` into one list. An empty slice returns [`List::EMPTY`]
    /// without touching the pool.
    pub fn push(&mut self, items: &[T]) -> List<T> {
        let start = self.items.len();
        self.items.extend_from_slice(items);
        self.seal(start).expect("list pool overflow")
    }
}

impl<T> Index<List<T>> for ListPool<T> {
    type Output = [T];

    #[inline]
    fn index(&self, list: List<T>) -> &[T] {
        &self.items[list.range()]
    }
}

impl<T> IndexMut<List<T>> for ListPool<T> {
    #[inline]
    fn index_mut(&mut self, list: List<T>) -> &mut [T] {
        &mut self.items[list.range()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_share_one_pool() {
        let mut pool = ListPool::new();
        let a = pool.push(&[1, 2]);
        let empty = pool.push(&[]);
        let b = pool.extend([3, 4, 5]);
        assert_eq!((&pool[a], &pool[b]), (&[1, 2][..], &[3, 4, 5][..]));
        assert_eq!((empty, empty.len(), pool.item_count()), (List::EMPTY, 0, 5));
        pool[b][0] = 30;
        assert_eq!(pool[b], [30, 4, 5]);
        assert_eq!(format!("{b:?}"), "List[2..5]");
        pool.clear();
        assert_eq!(pool.item_count(), 0);
    }

    #[test]
    fn nested_construction_through_a_scratch_stack() {
        // (1 (2 3) 4): the inner list is sealed before the outer one.
        let mut pool: ListPool<u32> = ListPool::new();
        let mut scratch = Vec::new();
        let outer = scratch.len();
        scratch.push(1);
        let inner = scratch.len();
        scratch.extend([2, 3]);
        let inner = pool.push_drain(&mut scratch, inner);
        scratch.push(100 + inner.len() as u32);
        scratch.push(4);
        let outer = pool.push_drain(&mut scratch, outer);
        assert!(scratch.is_empty());
        assert_eq!(
            (&pool[inner], &pool[outer]),
            (&[2, 3][..], &[1, 102, 4][..])
        );
    }
}
