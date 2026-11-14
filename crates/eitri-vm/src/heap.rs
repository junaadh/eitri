//! Non-moving storage for managed values.
//!
//! References retain pointer provenance and are never converted to integers.
//! Collection takes expected O(R + N + E) time for roots, objects and object
//! edges; hash collisions can make lookup quadratic. Its reusable worklist is
//! reserved when record objects are allocated, so collection does not allocate.

use eitri_base::FxHashMap;
use std::ptr::NonNull;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HeapRef(NonNull<Object>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObjectKind {
    String,
    Record,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RecordField {
    pub word: u64,
    pub reference: Option<HeapRef>,
}

#[derive(Debug)]
enum ObjectData {
    String(String),
    Record {
        shape: u32,
        fields: Vec<RecordField>,
    },
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
    data: ObjectData,
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
    mark_stack: Vec<usize>,
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

    pub fn case(
        &mut self,
        source: HeapRef,
        upper: bool,
        limit: usize,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<HeapRef, HeapLimit> {
        let length = self
            .string(source)
            .expect("live source")
            .chars()
            .try_fold(0usize, |n, c| {
                n.checked_add(simple_case(c, upper).len_utf8())
            })
            .ok_or(HeapLimit)?;
        self.reserve_object(length, limit, memory)?;
        let mut result = String::new();
        super::reserve_string_vm(&mut result, length, memory).map_err(|_| HeapLimit)?;
        for c in self.string(source).expect("live source").chars() {
            result.push(simple_case(c, upper));
        }
        self.finish_string(result, limit, memory)
    }

    pub fn replace(
        &mut self,
        source: HeapRef,
        from: HeapRef,
        to: HeapRef,
        limit: usize,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<HeapRef, HeapLimit> {
        let original = self.string(source).expect("live source");
        let pattern = self.string(from).expect("live pattern");
        let replacement = self.string(to).expect("live replacement");
        let count = original.match_indices(pattern).count();
        let length = original
            .len()
            .checked_sub(count.checked_mul(pattern.len()).ok_or(HeapLimit)?)
            .and_then(|n| n.checked_add(count.checked_mul(replacement.len())?))
            .ok_or(HeapLimit)?;
        self.reserve_object(length, limit, memory)?;
        let mut result = String::new();
        super::reserve_string_vm(&mut result, length, memory).map_err(|_| HeapLimit)?;
        let original = self.string(source).expect("live source");
        let pattern = self.string(from).expect("live pattern");
        let replacement = self.string(to).expect("live replacement");
        let mut last = 0;
        for (index, matched) in original.match_indices(pattern) {
            result.push_str(&original[last..index]);
            result.push_str(replacement);
            last = index + matched.len();
        }
        result.push_str(&original[last..]);
        self.finish_string(result, limit, memory)
    }

    pub fn slice(
        &mut self,
        reference: HeapRef,
        start: usize,
        end: usize,
        limit: usize,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<HeapRef, HeapLimit> {
        let length = end - start;
        self.reserve_object(length, limit, memory)?;
        let mut text = String::new();
        super::reserve_string_vm(&mut text, length, memory).map_err(|_| HeapLimit)?;
        text.push_str(&self.string(reference).expect("live string root")[start..end]);
        self.finish_string(text, limit, memory)
    }

    /// Render a verified plan after all operands have been evaluated. The
    /// first pass computes the exact byte length, and the second writes the
    /// result into its only payload allocation.
    pub fn interpolate(
        &mut self,
        plan: &str,
        words: &[u64],
        references: &[Option<HeapRef>],
        limit: usize,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<HeapRef, HeapLimit> {
        use eitri_bytecode::{FormatPart, ValueType, format_parts};
        let mut length = 0usize;
        let mut slot = 0usize;
        for part in format_parts(plan) {
            let n = match part.expect("verified interpolation plan") {
                FormatPart::Literal(text) => text.len(),
                FormatPart::Value(ValueType::String) => {
                    let reference = references[slot].expect("verified string operand");
                    slot += 1;
                    self.string(reference).expect("live string root").len()
                }
                FormatPart::Value(ty) => {
                    let value = super::format_scalar(super::format_type_op(ty), words[slot]);
                    slot += 1;
                    value.as_str().len()
                }
            };
            length = length.checked_add(n).ok_or(HeapLimit)?;
        }
        self.reserve_object(length, limit, memory)?;
        let mut text = String::new();
        super::reserve_string_vm(&mut text, length, memory).map_err(|_| HeapLimit)?;
        slot = 0;
        for part in format_parts(plan) {
            match part.expect("verified interpolation plan") {
                FormatPart::Literal(literal) => text.push_str(literal),
                FormatPart::Value(ValueType::String) => {
                    let reference = references[slot].expect("verified string operand");
                    slot += 1;
                    text.push_str(self.string(reference).expect("live string root"));
                }
                FormatPart::Value(ty) => {
                    let value = super::format_scalar(super::format_type_op(ty), words[slot]);
                    slot += 1;
                    text.push_str(value.as_str());
                }
            }
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
            data: ObjectData::String(text),
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

    /// Allocate a record after reserving its payload and collector worklist.
    pub fn alloc_record(
        &mut self,
        shape: u32,
        fields: impl ExactSizeIterator<Item = RecordField>,
        limit: usize,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<HeapRef, HeapLimit> {
        let payload = fields
            .len()
            .checked_mul(size_of::<RecordField>())
            .ok_or(HeapLimit)?;
        self.reserve_object(payload, limit, memory)?;
        let needed = self.objects.len() + 1;
        let additional = needed.saturating_sub(self.mark_stack.len());
        super::reserve_vm(&mut self.mark_stack, additional, memory).map_err(|_| HeapLimit)?;
        let mut values = Vec::new();
        super::reserve_vm(&mut values, fields.len(), memory).map_err(|_| HeapLimit)?;
        values.extend(fields);
        let payload = values
            .capacity()
            .checked_mul(size_of::<RecordField>())
            .ok_or(HeapLimit)?;
        let size = size_of::<Object>().checked_add(payload).ok_or(HeapLimit)?;
        let Some(bytes) = self.bytes.checked_add(size).filter(|&bytes| bytes <= limit) else {
            super::release_vm(payload, memory);
            return Err(HeapLimit);
        };
        if super::charge_vm(size_of::<Object>(), memory).is_err() {
            super::release_vm(payload, memory);
            return Err(HeapLimit);
        }
        let mut object = match Box::try_new(Object {
            header: Header {
                kind: ObjectKind::Record,
                size,
                marked: false,
            },
            data: ObjectData::Record {
                shape,
                fields: values,
            },
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

    pub fn record(&self, reference: HeapRef) -> Option<&[RecordField]> {
        match &self.objects.get(*self.index.get(&reference)?)?.data {
            ObjectData::Record { fields, .. } => Some(fields),
            ObjectData::String(_) => None,
        }
    }

    pub fn record_shape(&self, reference: HeapRef) -> Option<u32> {
        match &self.objects.get(*self.index.get(&reference)?)?.data {
            ObjectData::Record { shape, .. } => Some(*shape),
            ObjectData::String(_) => None,
        }
    }

    pub fn record_mut(&mut self, reference: HeapRef) -> Option<&mut [RecordField]> {
        match &mut self.objects.get_mut(*self.index.get(&reference)?)?.data {
            ObjectData::Record { fields, .. } => Some(fields),
            ObjectData::String(_) => None,
        }
    }

    /// Looks up a live reference without dereferencing an untrusted pointer.
    pub fn string(&self, reference: HeapRef) -> Option<&str> {
        let object = self.objects.get(*self.index.get(&reference)?)?;
        match &object.data {
            ObjectData::String(text) => Some(text.as_str()),
            ObjectData::Record { .. } => None,
        }
    }

    /// Compare immutable indirect boxes by contents, keeping class references
    /// as identity. Work and scratch are bounded by the execution and memory budgets.
    pub fn equal_records(
        &self,
        left: Option<HeapRef>,
        right: Option<HeapRef>,
        shapes: &[Box<[eitri_bytecode::ValueType]>],
        class_count: u32,
        budget: &mut u64,
        memory: &mut Option<eitri_base::MemoryBudget>,
    ) -> Result<bool, super::FaultKind> {
        let mut pending = Vec::new();
        let result = (|| {
            super::reserve_vm(&mut pending, 1, memory)?;
            pending.push((left, right));
            while let Some((left, right)) = pending.pop() {
                if *budget == 0 {
                    return Err(super::FaultKind::Budget);
                }
                *budget -= 1;
                let (left, right) = match (left, right) {
                    (None, None) => continue,
                    (Some(left), Some(right)) => (left, right),
                    _ => return Ok(false),
                };
                let left_shape = self
                    .record_shape(left)
                    .ok_or(super::FaultKind::InvalidEnumPayload)?;
                if self.record_shape(right) != Some(left_shape) {
                    return Ok(false);
                }
                let shape = shapes
                    .get(left_shape as usize)
                    .ok_or(super::FaultKind::InvalidEnumPayload)?;
                let left_fields = self
                    .record(left)
                    .ok_or(super::FaultKind::InvalidEnumPayload)?;
                let right_fields = self
                    .record(right)
                    .ok_or(super::FaultKind::InvalidEnumPayload)?;
                if left_fields.len() != shape.len() || right_fields.len() != shape.len() {
                    return Err(super::FaultKind::InvalidEnumPayload);
                }
                for ((left, right), ty) in left_fields.iter().zip(right_fields).zip(shape.iter()) {
                    use eitri_bytecode::ValueType;
                    let equal = match ty {
                        ValueType::String | ValueType::NullableString => {
                            match (left.reference, right.reference) {
                                (None, None) => true,
                                (Some(a), Some(b)) => self.string(a) == self.string(b),
                                _ => false,
                            }
                        }
                        ValueType::Record(id) | ValueType::NullableRecord(id)
                            if *id >= class_count =>
                        {
                            super::reserve_vm(&mut pending, 1, memory)?;
                            pending.push((left.reference, right.reference));
                            true
                        }
                        ValueType::Record(_) | ValueType::NullableRecord(_) => {
                            left.reference == right.reference
                        }
                        ValueType::F32 => {
                            f32::from_bits(left.word as u32) == f32::from_bits(right.word as u32)
                        }
                        ValueType::F64 => f64::from_bits(left.word) == f64::from_bits(right.word),
                        _ => left.word == right.word,
                    };
                    if !equal {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        })();
        super::release_vm(
            pending.capacity() * size_of::<(Option<HeapRef>, Option<HeapRef>)>(),
            memory,
        );
        result
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
            + self.mark_stack.capacity() * std::mem::size_of::<usize>()
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
        if let Some(&index) = self.index.get(&reference)
            && !self.objects[index].header.marked
        {
            self.objects[index].header.marked = true;
            if self.objects[index].header.kind == ObjectKind::Record {
                debug_assert!(self.mark_stack.len() < self.mark_stack.capacity());
                self.mark_stack.push(index);
            }
        }
    }

    fn mark_retained_and_sweep(&mut self, memory: &mut Option<eitri_base::MemoryBudget>) {
        for index in 0..self.retained.len() {
            if let Some(reference) = self.retained[index].reference {
                self.mark(reference);
            }
        }
        while let Some(index) = self.mark_stack.pop() {
            let length = match &self.objects[index].data {
                ObjectData::Record { fields, .. } => fields.len(),
                ObjectData::String(_) => unreachable!(),
            };
            for offset in 0..length {
                let reference = match &self.objects[index].data {
                    ObjectData::Record { fields, .. } => fields[offset].reference,
                    ObjectData::String(_) => unreachable!(),
                };
                if let Some(reference) = reference {
                    self.mark(reference);
                }
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

fn simple_case(c: char, upper: bool) -> char {
    let table = if upper {
        super::unicode_case::UPPER
    } else {
        super::unicode_case::LOWER
    };
    match table.binary_search_by_key(&(c as u32), |&(key, _)| key) {
        Ok(index) => char::from_u32(table[index].1).expect("valid Unicode case mapping"),
        Err(_) => c,
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

#[cfg(test)]
mod record_tests {
    use super::*;

    #[test]
    fn rooted_record_graph_survives_and_unrooted_cycles_are_collected() {
        let mut heap = Heap::default();
        let mut memory = None;
        let text = heap.alloc_string("kept", usize::MAX, &mut memory).unwrap();
        let child = heap
            .alloc_record(
                0,
                [
                    RecordField {
                        word: 5,
                        reference: Some(text),
                    },
                    RecordField {
                        word: 9,
                        reference: Some(text),
                    },
                ]
                .into_iter(),
                usize::MAX,
                &mut memory,
            )
            .unwrap();
        let parent = heap
            .alloc_record(
                0,
                [RecordField {
                    word: 7,
                    reference: Some(child),
                }]
                .into_iter(),
                usize::MAX,
                &mut memory,
            )
            .unwrap();
        heap.record_mut(child).unwrap()[0].reference = Some(parent);
        heap.collect([parent], &mut memory);
        assert_eq!(heap.record(parent).unwrap()[0].word, 7);
        assert_eq!(heap.record(child).unwrap()[0].word, 5);
        assert_eq!(heap.string(text), Some("kept"));
        heap.collect([], &mut memory);
        assert!(heap.record(parent).is_none());
        assert!(heap.record(child).is_none());
        assert!(heap.string(text).is_none());
    }
}
