//! Non-moving storage for managed strings.
//!
//! A reference retains its pointer provenance; it is never converted to an
//! integer. Collection uses expected O(R+N) time and O(R) temporary memory
//! for R roots and N objects; hash collisions can make it quadratic. Strings
//! contain no outgoing edges.

use eitri_base::{FxHashMap, FxHashSet};
use std::ptr::NonNull;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeapRef(NonNull<Object>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObjectKind {
    String,
}

#[derive(Debug)]
struct Header {
    kind: ObjectKind,
    size: usize,
    marked: bool,
}

#[derive(Debug)]
struct Object {
    header: Header,
    text: Box<str>,
}

/// A host-owned root token. Release it through the context that issued it.
/// The private allocation gives live tokens distinct identities without global
/// state; neither the collector nor the context dereferences that identity.
#[derive(Debug)]
pub struct RetainedString {
    token: Box<u8>,
    index: usize,
    generation: u64,
}

#[derive(Debug)]
struct RetainedSlot {
    identity: Option<NonNull<u8>>,
    reference: Option<HeapRef>,
    generation: u64,
}

/// A heap-local limit was exceeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapLimit;

/// A published frame and its pointer-preserving reference slots.
/// Scalar slots have `None`; a root-map bit selects a live reference slot.
pub struct RootFrame<'a> {
    pub map: &'a eitri_bytecode::RootMap,
    pub slots: &'a [Option<HeapRef>],
}

/// Root maps with dynamic recipes or out-of-frame slots cannot be used yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidRoots;

/// Owns objects for one runtime. Object addresses remain fixed until swept.
#[derive(Debug, Default)]
pub struct Heap {
    // Boxes keep object addresses stable when the index grows or is compacted.
    #[allow(clippy::vec_box)]
    objects: Vec<Box<Object>>,
    index: FxHashMap<HeapRef, usize>,
    retained: Vec<RetainedSlot>,
    free_retained: Vec<usize>,
    bytes: usize,
}

impl Heap {
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// The limit accounts for object headers, object fields and string bytes;
    /// vector capacity and allocator overhead are not included.
    pub fn alloc_string(&mut self, text: &str, limit: usize) -> Result<HeapRef, HeapLimit> {
        let size = std::mem::size_of::<Object>()
            .checked_add(text.len())
            .ok_or(HeapLimit)?;
        let bytes = self.bytes.checked_add(size).ok_or(HeapLimit)?;
        if bytes > limit {
            return Err(HeapLimit);
        }
        let mut object = Box::new(Object {
            header: Header {
                kind: ObjectKind::String,
                size,
                marked: false,
            },
            text: text.into(),
        });
        let reference = HeapRef(NonNull::from(object.as_mut()));
        self.index.insert(reference, self.objects.len());
        self.objects.push(object);
        self.bytes = bytes;
        Ok(reference)
    }

    /// Looks up a live reference without dereferencing an untrusted pointer.
    pub fn string(&self, reference: HeapRef) -> Option<&str> {
        let object = self.objects.get(*self.index.get(&reference)?)?;
        (object.header.kind == ObjectKind::String).then_some(object.text.as_ref())
    }

    /// Add a host-owned collector root; slots are generation-checked and
    /// recycled. A live token's unique allocation also identifies its context.
    pub fn retain_string(&mut self, reference: HeapRef) -> RetainedString {
        debug_assert!(self.string(reference).is_some());
        let token = Box::new(0u8);
        let identity = NonNull::from(token.as_ref());
        let index = self.free_retained.pop().unwrap_or_else(|| {
            self.retained.push(RetainedSlot {
                identity: None,
                reference: None,
                generation: 0,
            });
            self.retained.len() - 1
        });
        let slot = &mut self.retained[index];
        slot.identity = Some(identity);
        slot.reference = Some(reference);
        RetainedString {
            token,
            index,
            generation: slot.generation,
        }
    }

    pub fn retained_string(&self, handle: &RetainedString) -> Option<HeapRef> {
        let slot = self.retained.get(handle.index)?;
        (slot.generation == handle.generation
            && slot.identity == Some(NonNull::from(handle.token.as_ref())))
        .then_some(slot.reference)
        .flatten()
    }

    pub fn release_string(&mut self, handle: &RetainedString) -> bool {
        if self.retained_string(handle).is_none() {
            return false;
        }
        let slot = &mut self.retained[handle.index];
        slot.identity = None;
        slot.reference = None;
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            self.free_retained.push(handle.index);
        }
        true
    }

    pub fn retained_metadata_bytes(&self) -> usize {
        self.retained.capacity() * std::mem::size_of::<RetainedSlot>()
            + self.free_retained.capacity() * std::mem::size_of::<usize>()
    }

    /// Collect from ordinary root-map bits for each published frame. Invalid
    /// metadata is rejected before collection, leaving the heap untouched.
    /// Dynamic and guarded values are not executable in this stage.
    pub fn collect_frames(
        &mut self,
        frames: &[RootFrame<'_>],
        extra: &[HeapRef],
    ) -> Result<(), InvalidRoots> {
        let mut roots = extra.to_vec();
        for frame in frames {
            if !frame.map.dynamic.is_empty() || !frame.map.guarded.is_empty() {
                return Err(InvalidRoots);
            }
            for (word_index, &word) in frame.map.ordinary.iter().enumerate() {
                let mut bits = word;
                while bits != 0 {
                    let bit = bits.trailing_zeros() as usize;
                    let index = word_index
                        .checked_mul(64)
                        .and_then(|n| n.checked_add(bit))
                        .ok_or(InvalidRoots)?;
                    let value = frame.slots.get(index).ok_or(InvalidRoots)?;
                    if let Some(value) = *value {
                        roots.push(value);
                    }
                    bits &= bits - 1;
                }
            }
        }
        self.collect(roots);
        Ok(())
    }

    /// Marks and sweeps objects not present in `roots`.
    pub fn collect(&mut self, roots: impl IntoIterator<Item = HeapRef>) {
        let mut roots: FxHashSet<_> = roots.into_iter().collect();
        roots.extend(self.retained.iter().filter_map(|slot| slot.reference));
        for object in &mut self.objects {
            object.header.marked = roots.contains(&HeapRef(NonNull::from(object.as_mut())));
        }
        self.objects.retain(|object| {
            if object.header.marked {
                true
            } else {
                self.bytes -= object.header.size;
                false
            }
        });
        self.index.clear();
        for (index, object) in self.objects.iter().enumerate() {
            self.index
                .insert(HeapRef(NonNull::from(object.as_ref())), index);
        }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        {
            let leaked = self
                .retained
                .iter()
                .filter(|slot| slot.reference.is_some())
                .count();
            if leaked > 0 {
                eprintln!("eitri: {leaked} retained string handle(s) were not released");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_strings_survive_and_unreachable_strings_are_reclaimed() {
        let mut heap = Heap::default();
        let baseline = heap.bytes();
        let live = heap.alloc_string("hello", usize::MAX).unwrap();
        let dead = heap.alloc_string("world", usize::MAX).unwrap();
        heap.collect([live]);
        assert_eq!(heap.string(live), Some("hello"));
        assert_eq!(heap.string(dead), None);
        heap.collect([]);
        assert_eq!(heap.bytes(), baseline);
    }

    #[test]
    fn published_frame_maps_trace_only_selected_slots() {
        let mut heap = Heap::default();
        let live = heap.alloc_string("live", usize::MAX).unwrap();
        let dead = heap.alloc_string("dead", usize::MAX).unwrap();
        let map = eitri_bytecode::RootMap {
            ordinary: vec![0b10],
            ..Default::default()
        };
        let slots = [Some(dead), Some(live)];
        heap.collect_frames(
            &[RootFrame {
                map: &map,
                slots: &slots,
            }],
            &[],
        )
        .unwrap();
        assert_eq!(heap.string(live), Some("live"));
        assert_eq!(heap.string(dead), None);
        let bad = eitri_bytecode::RootMap {
            ordinary: vec![0b100],
            ..Default::default()
        };
        let before = heap.bytes();
        assert_eq!(
            heap.collect_frames(
                &[RootFrame {
                    map: &bad,
                    slots: &slots
                }],
                &[]
            ),
            Err(InvalidRoots)
        );
        assert_eq!(heap.bytes(), before);
    }

    #[test]
    fn limit_failure_does_not_change_heap() {
        let mut heap = Heap::default();
        assert_eq!(heap.alloc_string("hello", 0), Err(HeapLimit));
        assert_eq!(heap.bytes(), 0);
    }
}
