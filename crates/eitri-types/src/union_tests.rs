use super::*;
use crate::{ByteRange, CopyKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cell {
    Uninitialized,
    Poison,
    Scalar(u64),
    Reference(Option<u32>),
    Type(bool),
}
#[derive(Clone)]
enum Value {
    Scalar(u64),
    Reference(u32),
    Product(Vec<Value>),
    Array(Vec<Value>),
    Case(usize, Option<Box<Value>>),
    Dynamic(bool, u32),
}
fn some(v: Value) -> Value {
    Value::Case(1, Some(Box::new(v)))
}
fn case(n: usize, v: Value) -> Value {
    Value::Case(n, Some(Box::new(v)))
}
fn build(nodes: &[Shape<'_>], root: usize, domain: StorageDomain) -> LayoutStore {
    LayoutStore::build(nodes.len(), root, domain, |id| Ok(nodes[id])).unwrap()
}
// Independent semantic oracle: no layout, transfer or trace descriptor access.
fn labels(value: &Value, out: &mut Vec<u32>) {
    match value {
        Value::Reference(label) | Value::Dynamic(true, label) => out.push(*label),
        Value::Product(fields) | Value::Array(fields) => {
            for v in fields {
                labels(v, out);
            }
        }
        Value::Case(_, Some(v)) => labels(v, out),
        _ => {}
    }
}
fn copy(source: &[Cell], from: usize, dest: &mut [Cell], to: usize, count: usize) {
    assert!(
        !source[from..from + count].contains(&Cell::Poison),
        "read inactive pair"
    );
    dest[to..to + count].copy_from_slice(&source[from..from + count]);
}
#[derive(Clone, Copy)]
enum Source {
    Native(usize),
    Packed(usize, usize),
}
fn tag(storage: &[Cell], offset: usize) -> usize {
    let Cell::Scalar(n @ 0..=1) = storage[offset] else {
        panic!("invalid tag")
    };
    n as usize
}
fn pack(
    store: &LayoutStore,
    id: LayoutId,
    source: &[Cell],
    dest: &mut [Cell],
    refs: usize,
    scalars: usize,
) {
    enum Task {
        Value(LayoutId, Source, usize, usize),
        Tag(usize, usize, usize),
    }
    let mut stack = vec![Task::Value(id, Source::Native(0), refs, scalars)];
    while let Some(task) = stack.pop() {
        let Task::Value(id, src, refs, scalars) = task else {
            let Task::Tag(from, to, count) = task else {
                unreachable!()
            };
            copy(source, from, dest, to, count);
            continue;
        };
        let plan = &store.get(id).unwrap().copy;
        let scalar_source = match src {
            Source::Native(n) | Source::Packed(_, n) => n,
        };
        let reference_source = match src {
            Source::Native(n) | Source::Packed(n, _) => n,
        };
        match &plan.kind {
            CopyKind::Dynamic | CopyKind::Scalar => {
                copy(source, scalar_source, dest, scalars, plan.scalar_bytes)
            }
            CopyKind::Reference => copy(source, reference_source, dest, refs, plan.reference_bytes),
            CopyKind::Product(fields) => {
                for f in fields {
                    let src = match src {
                        Source::Native(n) => Source::Native(n + f.source),
                        Source::Packed(r, s) => Source::Packed(r + f.references, s + f.scalars),
                    };
                    stack.push(Task::Value(
                        f.layout,
                        src,
                        refs + f.references,
                        scalars + f.scalars,
                    ));
                }
            }
            CopyKind::Repeat {
                count,
                source_stride,
                reference_stride,
                scalar_stride,
                element,
            } => {
                for n in 0..*count as usize {
                    let src = match src {
                        Source::Native(base) => Source::Native(base + n * source_stride),
                        Source::Packed(r, s) => {
                            Source::Packed(r + n * reference_stride, s + n * scalar_stride)
                        }
                    };
                    stack.push(Task::Value(
                        *element,
                        src,
                        refs + n * reference_stride,
                        scalars + n * scalar_stride,
                    ));
                }
            }
            CopyKind::Split {
                reference_source,
                tag: range,
                scalar_source,
                scalar_destination,
                cases,
            } => {
                let (tag_source, r, s) = match src {
                    Source::Native(n) => {
                        (n + range.offset, n + reference_source, n + scalar_source)
                    }
                    Source::Packed(r, s) => (s, r, s + scalar_destination),
                };
                let active = tag(source, tag_source);
                dest[refs..refs + plan.reference_bytes].fill(Cell::Reference(None));
                // LIFO: publish only after the selected payload has copied.
                stack.push(Task::Tag(tag_source, scalars, range.bytes));
                if let Some(child) = cases[active] {
                    stack.push(Task::Value(
                        child,
                        Source::Packed(r, s),
                        refs,
                        scalars + scalar_destination,
                    ));
                }
            }
        }
    }
}
fn init(
    store: &LayoutStore,
    id: LayoutId,
    tag: usize,
    payload: Option<&[Cell]>,
    dest: &mut [Cell],
) {
    let union = store.get(id).unwrap().union.as_ref().unwrap();
    let case = &union.cases[tag];
    let ByteRange { offset, bytes } = case.init.null_references;
    dest[offset..offset + bytes].fill(Cell::Reference(None));
    if let Some(transfer) = &case.transfer {
        assert_eq!(case.payload, Some(transfer.layout));
        assert_eq!(transfer.source, 0);
        pack(
            store,
            transfer.layout,
            payload.unwrap(),
            dest,
            transfer.references,
            transfer.scalars,
        );
    }
    dest[union.tag.offset..union.tag.offset + union.tag.bytes]
        .fill(Cell::Scalar(case.init.tag as u64));
}
fn encode(store: &LayoutStore, id: LayoutId, value: &Value) -> Vec<Cell> {
    let layout = store.get(id).unwrap();
    let mut storage = vec![Cell::Uninitialized; layout.size];
    if layout.copy.has_dynamic && layout.union.is_some() {
        storage.fill(Cell::Poison);
    }
    match value {
        Value::Scalar(n) => storage.fill(Cell::Scalar(*n)),
        Value::Reference(n) => storage.fill(Cell::Reference(Some(*n))),
        Value::Product(values) => {
            for (field, v) in layout.fields.iter().zip(values) {
                let src = encode(store, field.layout, v);
                // Fixture assembly places opaque bytes, including poison; the
                // plan interpreter below is what must never inspect/copy them.
                storage[field.offset..field.offset + src.len()].copy_from_slice(&src);
            }
        }
        Value::Array(values) => {
            let CopyKind::Repeat {
                element,
                source_stride,
                count,
                ..
            } = layout.copy.kind
            else {
                panic!("array plan")
            };
            assert_eq!(values.len(), count as usize);
            for (n, v) in values.iter().enumerate() {
                let src = encode(store, element, v);
                storage[n * source_stride..n * source_stride + src.len()].copy_from_slice(&src);
            }
        }
        Value::Case(tag, payload) => {
            let case = &layout.union.as_ref().unwrap().cases[*tag];
            let src = payload
                .as_ref()
                .map(|v| encode(store, case.payload.unwrap(), v));
            init(store, id, *tag, src.as_deref(), &mut storage);
        }
        Value::Dynamic(is_ref, label) => {
            storage[0..8].fill(Cell::Type(*is_ref));
            storage[8..16].fill(if *is_ref {
                Cell::Reference(Some(*label))
            } else {
                Cell::Scalar(*label as u64)
            });
        }
    }
    storage
}
fn roots(store: &LayoutStore, storage: &[Cell]) -> Vec<u32> {
    let mut result = Vec::new();
    let mut stack = vec![(store.root(), 0usize, None)];
    let mut reference = |offset: usize| match storage[offset] {
        Cell::Reference(Some(label)) => result.push(label),
        Cell::Reference(None) => {}
        other => panic!("scalar visited as reference: {other:?} at {offset}"),
    };
    while let Some((id, base, packed)) = stack.pop() {
        if let Some((r, s)) = packed {
            match &store.get(id).unwrap().copy.kind {
                CopyKind::Dynamic => {
                    let TraceDesc::Dynamic(recipe) = &store.get(id).unwrap().trace else {
                        panic!("dynamic recipe")
                    };
                    match storage[s + recipe.type_slot as usize * 8] {
                        Cell::Type(true) => reference(s + recipe.payload_slot as usize * 8),
                        Cell::Type(false) => {}
                        _ => panic!("read inactive pair"),
                    }
                }
                CopyKind::Product(fields) => stack.extend(
                    fields
                        .iter()
                        .map(|f| (f.layout, 0, Some((r + f.references, s + f.scalars)))),
                ),
                CopyKind::Repeat {
                    count,
                    reference_stride,
                    scalar_stride,
                    element,
                    ..
                } => stack.extend((0..*count as usize).map(|n| {
                    (
                        *element,
                        0,
                        Some((r + n * reference_stride, s + n * scalar_stride)),
                    )
                })),
                CopyKind::Split {
                    scalar_destination,
                    cases,
                    ..
                } => {
                    if let Some(child) = cases[tag(storage, s)] {
                        stack.push((child, 0, Some((r, s + scalar_destination))));
                    }
                }
                _ => {}
            }
            continue;
        }
        match &store.get(id).unwrap().trace {
            TraceDesc::Guarded {
                tag: range,
                ordinary,
                stride,
                cases,
            } => {
                let active = tag(storage, base + range.offset);
                for n in 0..ordinary.bytes / stride {
                    reference(base + ordinary.offset + n * stride);
                }
                if let Some(case) = cases[active] {
                    stack.push((
                        case.layout,
                        0,
                        Some((base + case.references, base + case.scalars)),
                    ));
                }
            }
            TraceDesc::None => {}
            TraceDesc::Reference => reference(base),
            TraceDesc::ReferencePartition {
                offset,
                bytes,
                stride,
            } => {
                for n in 0..bytes / stride {
                    reference(base + offset + n * stride);
                }
            }
            TraceDesc::Product(fields) => {
                stack.extend(fields.iter().map(|f| (f.layout, base + f.offset, None)))
            }
            TraceDesc::Repeat {
                count,
                stride,
                element,
            } => stack.extend((0..*count as usize).map(|n| (*element, base + n * stride, None))),
            TraceDesc::Dynamic(recipe) => match storage[base + recipe.type_slot as usize * 8] {
                Cell::Type(true) => reference(base + recipe.payload_slot as usize * 8),
                Cell::Type(false) => {}
                _ => panic!("uninitialized dynamic type"),
            },
        }
    }
    result.sort_unstable();
    result
}
fn check(store: &LayoutStore, value: &Value) -> Vec<Cell> {
    let storage = encode(store, store.root(), value);
    let mut expected = Vec::new();
    labels(value, &mut expected);
    expected.sort_unstable();
    assert_eq!(roots(store, &storage), expected);
    storage
}

#[test]
fn every_anonymous_case_and_nested_ordering() {
    let nodes = [
        Shape::Scalar(ScalarKind::I64),
        Shape::ManagedReference(0),
        Shape::Optional(1),
        Shape::Result(1, 0),
        Shape::Optional(3),
        Shape::Result(1, 2),
        Shape::Product(&[0, 2, 3]),
        Shape::Array(2, 6),
        Shape::Optional(7),
    ];
    for domain in [StorageDomain::Frame, StorageDomain::Heap] {
        let optional_result = build(&nodes, 4, domain);
        for v in [
            Value::Case(0, None),
            some(case(0, Value::Reference(7))),
            some(case(1, Value::Scalar(u64::MAX))),
        ] {
            check(&optional_result, &v);
        }
        assert!(
            optional_result
                .get(optional_result.root())
                .unwrap()
                .union
                .as_ref()
                .unwrap()
                .cases[0]
                .payload
                .is_none()
        );
        let result_optional = build(&nodes, 5, domain);
        assert!(
            result_optional
                .get(result_optional.root())
                .unwrap()
                .union
                .as_ref()
                .unwrap()
                .cases[0]
                .payload
                .is_some()
        );
        for v in [
            case(0, Value::Reference(7)),
            case(1, Value::Case(0, None)),
            case(1, some(Value::Reference(9))),
        ] {
            check(&result_optional, &v);
        }
        let composed = build(&nodes, 7, domain);
        let value = Value::Array(vec![
            Value::Product(vec![
                Value::Scalar(7),
                some(Value::Reference(11)),
                case(0, Value::Reference(12)),
            ]),
            Value::Product(vec![
                Value::Scalar(12),
                Value::Case(0, None),
                case(1, Value::Scalar(11)),
            ]),
        ]);
        check(&composed, &value);
        let outer = build(&nodes, 8, domain);
        check(&outer, &Value::Case(0, None));
        check(&outer, &some(value));
    }
    let mut types = TypeStore::default();
    let scalar = types.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
    let optional = types.intern(TypeDesc::Optional(scalar)).unwrap();
    let result = types
        .intern(TypeDesc::Result {
            error: scalar,
            value: optional,
        })
        .unwrap();
    let store = LayoutStore::for_type(&types, result, StorageDomain::Frame).unwrap();
    assert!(store.get(store.root()).unwrap().union.is_some());
    assert!(matches!(
        LayoutStore::for_type(&types, result, StorageDomain::ExternC),
        Err(LayoutError::Unsupported)
    ));
}

#[test]
fn transitions_clear_all_inactive_references_and_preserve_full_scalar_range() {
    let nodes = [
        Shape::ManagedReference(0),
        Shape::Scalar(ScalarKind::I64),
        Shape::Product(&[0, 1, 0]),
        Shape::Result(2, 1),
        Shape::Optional(1),
    ];
    for domain in [StorageDomain::Heap, StorageDomain::Frame] {
        let store = build(&nodes, 3, domain);
        let union = store.get(store.root()).unwrap().union.as_ref().unwrap();
        let mut dest = vec![Cell::Reference(Some(999)); store.get(store.root()).unwrap().size];
        for tag in [0, 1, 0, 1] {
            let v = if tag == 0 {
                Value::Product(vec![
                    Value::Reference(1),
                    Value::Scalar(1),
                    Value::Reference(2),
                ])
            } else {
                Value::Scalar(u64::MAX)
            };
            let payload = encode(&store, union.cases[tag].payload.unwrap(), &v);
            init(&store, store.root(), tag, Some(&payload), &mut dest);
            assert_eq!(
                roots(&store, &dest),
                if tag == 0 { vec![1, 2] } else { vec![] }
            );
            if tag == 1 {
                assert!(
                    dest[union.references.offset..union.references.offset + union.references.bytes]
                        .iter()
                        .all(|c| *c == Cell::Reference(None))
                );
            }
        }
        let optional = build(&nodes, 4, domain);
        let u = optional
            .get(optional.root())
            .unwrap()
            .union
            .as_ref()
            .unwrap();
        for bits in [0, i64::MIN as u64, i64::MAX as u64, u64::MAX] {
            let storage = check(&optional, &some(Value::Scalar(bits)));
            assert_eq!(storage[u.scalars.offset], Cell::Scalar(bits));
            assert_eq!(storage[u.tag.offset], Cell::Scalar(1));
        }
        check(&optional, &Value::Case(0, None));
    }
}

#[test]
fn dynamic_recipes_compose_through_active_cases() {
    let nodes = [
        Shape::Dynamic,
        Shape::Scalar(ScalarKind::I64),
        Shape::Product(&[1, 0]),
        Shape::Array(2, 2),
        Shape::Optional(0),
        Shape::Result(1, 3),
    ];
    for domain in [StorageDomain::Heap, StorageDomain::Frame] {
        let store = build(&nodes, 3, domain);
        check(
            &store,
            &Value::Array(vec![
                Value::Product(vec![Value::Scalar(7), Value::Dynamic(true, 7)]),
                Value::Product(vec![Value::Scalar(7), Value::Dynamic(false, 7)]),
            ]),
        );
        let optional = build(&nodes, 4, domain);
        check(&optional, &Value::Case(0, None));
        check(&optional, &some(Value::Dynamic(true, 7)));
        let result = build(&nodes, 5, domain);
        check(&result, &case(0, Value::Scalar(7)));
        check(
            &result,
            &case(
                1,
                Value::Array(vec![
                    Value::Product(vec![Value::Scalar(7), Value::Dynamic(true, 8)]),
                    Value::Product(vec![Value::Scalar(7), Value::Dynamic(false, 8)]),
                ]),
            ),
        );
    }
}

#[test]
fn large_union_arrays_retain_only_graph_sized_plans() {
    let nodes = [
        Shape::ManagedReference(0),
        Shape::Scalar(ScalarKind::I64),
        Shape::Result(0, 1),
        Shape::Array(1_000_000, 2),
        Shape::Optional(3),
    ];
    let store = build(&nodes, 4, StorageDomain::Frame);
    assert_eq!(store.layouts.iter().flatten().count(), nodes.len());
    let array = store.get(LayoutId(3)).unwrap();
    assert!(matches!(
        array.copy.kind,
        CopyKind::Repeat {
            count: 1_000_000,
            ..
        }
    ));
    assert!(matches!(
        array.trace,
        TraceDesc::Repeat {
            count: 1_000_000,
            ..
        }
    ));
    assert_eq!(
        store
            .get(store.root())
            .unwrap()
            .union
            .as_ref()
            .unwrap()
            .cases
            .len(),
        2
    );
}

#[test]
fn dynamic_transitions_and_nested_copies_never_read_inactive_garbage() {
    let nodes = [
        Shape::Dynamic,
        Shape::Scalar(ScalarKind::I64),
        Shape::ManagedReference(0),
        Shape::Optional(0),
        Shape::Product(&[1, 2, 3]),
        Shape::Result(4, 1),
        Shape::Optional(5),
        Shape::Array(2, 6),
        Shape::Result(7, 1),
        Shape::Optional(8),
    ];
    let active = Value::Array(vec![
        some(case(
            0,
            Value::Product(vec![
                Value::Scalar(13),
                Value::Reference(12),
                some(Value::Dynamic(true, 13)),
            ]),
        )),
        some(case(1, Value::Scalar(13))),
    ]);
    for domain in [StorageDomain::Frame, StorageDomain::Heap] {
        let store = build(&nodes, 8, domain);
        let layout = store.get(store.root()).unwrap();
        let union = layout.union.as_ref().unwrap();
        let mut storage = vec![Cell::Poison; layout.size];
        for tag in [0, 1, 0, 1] {
            let value = if tag == 0 {
                active.clone()
            } else {
                Value::Scalar(13)
            };
            let source = encode(&store, union.cases[tag].payload.unwrap(), &value);
            init(&store, store.root(), tag, Some(&source), &mut storage);
            if tag == 1 {
                // The scalar case occupies only its eight bytes. All remaining
                // scalar/dynamic bytes are inactive and would fault if read.
                storage[union.scalars.offset + 8..union.scalars.offset + union.scalars.bytes]
                    .fill(Cell::Poison);
            }
            assert_eq!(storage[union.tag.offset], Cell::Scalar(tag as u64));
            let mut expected = Vec::new();
            labels(&value, &mut expected);
            expected.sort_unstable();
            assert_eq!(roots(&store, &storage), expected);
        }
        let outer = build(&nodes, 9, domain);
        check(&outer, &some(case(0, active.clone())));
        // Copying a nested scalar case must not bulk-copy its poisoned pairs.
        check(&outer, &some(case(1, Value::Scalar(13))));
        check(&outer, &Value::Case(0, None));
    }
}
