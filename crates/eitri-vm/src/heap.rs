//! Non-moving storage for managed strings.
//!
//! A reference retains its pointer provenance; it is never converted to an
//! integer. Collection uses expected O(R+N) time and O(1) temporary memory
//! for R roots and N objects; hash collisions can make it quadratic. Strings
//! contain no outgoing edges.

use eitri_base::FxHashMap;
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
    text: String,
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
#[derive(Clone, Copy)]
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

    /// Retained heap and index bytes, with a conservative index-table charge.
    pub fn budget_bytes(&self) -> Result<usize, eitri_base::OutOfMemory> {
        self.bytes
            .checked_add(self.objects.capacity() * size_of::<Box<Object>>())
            .and_then(|n| n.checked_add(self.retained_metadata_bytes()))
            .and_then(|n| {
                n.checked_add(
                    eitri_base::MemoryBudget::hash_table_bytes::<(HeapRef, usize)>(
                        self.index.capacity(),
                    )
                    .ok()?,
                )
            })
            .ok_or(eitri_base::OutOfMemory)
    }

    /// Allocate a string. Managed-object and full-runtime quotas are charged
    /// before the corresponding fallible allocation.
    pub fn alloc_string(
        &mut self,
        text: &str,
        limit: usize,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<HeapRef, HeapLimit> {
        let length = text.len();
        self.reserve_object(length, limit, memory)?;
        let mut contents = String::new();
        super::reserve_string_vm(&mut contents, length, memory).map_err(|_| HeapLimit)?;
        contents.push_str(text);
        self.finish_string(contents, limit, memory)
    }

    /// Concatenate two live strings in one allocation for the payload.
    pub fn concat(
        &mut self,
        left: HeapRef,
        right: HeapRef,
        limit: usize,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<HeapRef, HeapLimit> {
        let length = self
            .string(left)
            .expect("live left string")
            .len()
            .checked_add(self.string(right).expect("live right string").len())
            .ok_or(HeapLimit)?;
        self.reserve_object(length, limit, memory)?;
        let mut text = String::new();
        super::reserve_string_vm(&mut text, length, memory).map_err(|_| HeapLimit)?;
        text.push_str(self.string(left).expect("live left string"));
        text.push_str(self.string(right).expect("live right string"));
        self.finish_string(text, limit, memory)
    }

    /// Join a verified contiguous window of live string references with one
    /// allocation for the result payload.
    pub fn join(
        &mut self,
        parts: &[Option<HeapRef>],
        limit: usize,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<HeapRef, HeapLimit> {
        let mut length = 0usize;
        for part in parts {
            let reference = part.expect("verified string operand");
            length = length
                .checked_add(self.string(reference).expect("live string root").len())
                .ok_or(HeapLimit)?;
        }
        self.reserve_object(length, limit, memory)?;
        let mut text = String::new();
        super::reserve_string_vm(&mut text, length, memory).map_err(|_| HeapLimit)?;
        for part in parts {
            text.push_str(
                self.string(part.expect("verified string operand"))
                    .expect("live string root"),
            );
        }
        self.finish_string(text, limit, memory)
    }

    fn reserve_object(
        &mut self,
        text_len: usize,
        limit: usize,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<(), HeapLimit> {
        let size = size_of::<Object>().checked_add(text_len).ok_or(HeapLimit)?;
        if self.bytes.checked_add(size).is_none_or(|n| n > limit) {
            return Err(HeapLimit);
        }
        super::reserve_vm(&mut self.objects, 1, memory).map_err(|_| HeapLimit)?;
        super::reserve_map_vm(&mut self.index, 1, memory).map_err(|_| HeapLimit)?;
        Ok(())
    }

    fn finish_string(
        &mut self,
        text: String,
        limit: usize,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<HeapRef, HeapLimit> {
        let text_bytes = text.capacity();
        let Some((size, bytes)) = size_of::<Object>()
            .checked_add(text_bytes)
            .and_then(|size| self.bytes.checked_add(size).map(|bytes| (size, bytes)))
        else {
            super::release_vm(text_bytes, memory);
            return Err(HeapLimit);
        };
        if bytes > limit {
            super::release_vm(text_bytes, memory);
            return Err(HeapLimit);
        }
        if super::charge_vm(size_of::<Object>(), memory).is_err() {
            super::release_vm(text_bytes, memory);
            return Err(HeapLimit);
        }
        let mut object = match Box::try_new(Object {
            header: Header {
                kind: ObjectKind::String,
                size,
                marked: false,
            },
            text,
        }) {
            Ok(object) => object,
            Err(_) => {
                super::release_vm(size, memory);
                return Err(HeapLimit);
            }
        };
        let reference = HeapRef(NonNull::from(object.as_mut()));
        self.index.insert(reference, self.objects.len());
        self.objects.push(object);
        self.bytes = bytes;
        Ok(reference)
    }

    /// Looks up a live reference without dereferencing an untrusted pointer.
    pub fn string(&self, reference: HeapRef) -> Option<&str> {
        let object = self.objects.get(*self.index.get(&reference)?)?;
        (object.header.kind == ObjectKind::String).then_some(object.text.as_str())
    }

    /// Add a host-owned collector root; slots are generation-checked and
    /// recycled. A live token's unique allocation also identifies its context.
    pub fn retain_string(
        &mut self,
        reference: HeapRef,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<RetainedString, HeapLimit> {
        debug_assert!(self.string(reference).is_some());
        super::charge_vm(size_of::<u8>(), memory).map_err(|_| HeapLimit)?;
        let result = self.retain_string_charged(reference, memory);
        super::release_vm(size_of::<u8>(), memory);
        result
    }

    fn retain_string_charged(
        &mut self,
        reference: HeapRef,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<RetainedString, HeapLimit> {
        let token = Box::try_new(0u8).map_err(|_| HeapLimit)?;
        let identity = NonNull::from(token.as_ref());
        let index = if let Some(index) = self.free_retained.pop() {
            index
        } else {
            super::reserve_vm(&mut self.retained, 1, memory).map_err(|_| HeapLimit)?;
            // Every allocated retained slot can later be released without an
            // allocation, including when many handles are released together.
            let needed = self.retained.len() + 1;
            let additional = needed.saturating_sub(self.free_retained.len());
            super::reserve_vm(&mut self.free_retained, additional, memory)
                .map_err(|_| HeapLimit)?;
            self.retained.push(RetainedSlot {
                identity: None,
                reference: None,
                generation: 0,
            });
            self.retained.len() - 1
        };
        let slot = &mut self.retained[index];
        slot.identity = Some(identity);
        slot.reference = Some(reference);
        Ok(RetainedString {
            token,
            index,
            generation: slot.generation,
        })
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

    /// Validate every frame before marking, so malformed metadata cannot
    /// partially mutate the heap. Iteration and marking allocate no memory.
    pub fn collect_frames<'a>(
        &mut self,
        frames: impl Iterator<Item = RootFrame<'a>> + Clone,
        extra: &[HeapRef],
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<(), InvalidRoots> {
        for frame in frames.clone() {
            if !frame.map.dynamic.is_empty() || !frame.map.guarded.is_empty() {
                return Err(InvalidRoots);
            }
            for (word_index, &word) in frame.map.ordinary.iter().enumerate() {
                if word != 0 {
                    let index = word_index
                        .checked_mul(64)
                        .and_then(|n| n.checked_add(63 - word.leading_zeros() as usize))
                        .ok_or(InvalidRoots)?;
                    if index >= frame.slots.len() {
                        return Err(InvalidRoots);
                    }
                }
            }
        }
        for frame in frames {
            for (word_index, &word) in frame.map.ordinary.iter().enumerate() {
                let mut bits = word;
                while bits != 0 {
                    let index = word_index * 64 + bits.trailing_zeros() as usize;
                    if let Some(reference) = frame.slots[index] {
                        self.mark(reference);
                    }
                    bits &= bits - 1;
                }
            }
        }
        for &reference in extra {
            self.mark(reference);
        }
        self.mark_retained_and_sweep(memory);
        Ok(())
    }

    fn mark(&mut self, reference: HeapRef) {
        if let Some(&index) = self.index.get(&reference) {
            self.objects[index].header.marked = true;
        }
    }

    fn mark_retained_and_sweep(&mut self, memory: &mut Option<eitri_base::MemoryBudget>) {
        for index in 0..self.retained.len() {
            if let Some(reference) = self.retained[index].reference {
                self.mark(reference);
            }
        }
        self.objects.retain_mut(|object| {
            if object.header.marked {
                object.header.marked = false;
                true
            } else {
                self.bytes -= object.header.size;
                super::release_vm(object.header.size, memory);
                false
            }
        });
        // Capacity cannot shrink on clear, and survivors cannot exceed the
        // previous entry count, so reinsertion cannot grow the index.
        self.index.clear();
        for (index, object) in self.objects.iter().enumerate() {
            self.index
                .insert(HeapRef(NonNull::from(object.as_ref())), index);
        }
    }

    /// Marks and sweeps objects not present in `roots`, without allocation.
    pub fn collect(
        &mut self,
        roots: impl IntoIterator<Item = HeapRef>,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) {
        for reference in roots {
            self.mark(reference);
        }
        self.mark_retained_and_sweep(memory);
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
        let live = heap.alloc_string("hello", usize::MAX, &mut None).unwrap();
        let dead = heap.alloc_string("world", usize::MAX, &mut None).unwrap();
        heap.collect([live], &mut None);
        assert_eq!(heap.string(live), Some("hello"));
        assert_eq!(heap.string(dead), None);
        heap.collect([], &mut None);
        assert_eq!(heap.bytes(), baseline);
    }

    #[test]
    fn published_frame_maps_trace_only_selected_slots() {
        let mut heap = Heap::default();
        let live = heap.alloc_string("live", usize::MAX, &mut None).unwrap();
        let dead = heap.alloc_string("dead", usize::MAX, &mut None).unwrap();
        let map = eitri_bytecode::RootMap {
            ordinary: vec![0b10],
            ..Default::default()
        };
        let slots = [Some(dead), Some(live)];
        heap.collect_frames(
            [RootFrame {
                map: &map,
                slots: &slots,
            }]
            .into_iter(),
            &[],
            &mut None,
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
                [RootFrame {
                    map: &bad,
                    slots: &slots
                }]
                .into_iter(),
                &[],
                &mut None
            ),
            Err(InvalidRoots)
        );
        assert_eq!(heap.bytes(), before);
    }

    #[test]
    fn limit_failure_does_not_change_heap() {
        let mut heap = Heap::default();
        assert_eq!(heap.alloc_string("hello", 0, &mut None), Err(HeapLimit));
        assert_eq!(heap.bytes(), 0);
    }
}
