use super::*;
use crate::{ByteRange, CopyKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cell {
    Uninitialized,
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
    dest[to..to + count].copy_from_slice(&source[from..from + count]);
}
fn pack(
    store: &LayoutStore,
    id: LayoutId,
    source: &[Cell],
    dest: &mut [Cell],
    refs: usize,
    scalars: usize,
) {
    let mut stack = vec![(id, 0usize, refs, scalars)];
    while let Some((id, source_base, refs, scalars)) = stack.pop() {
        let plan = &store.get(id).unwrap().copy;
        assert!(!plan.has_dynamic, "dynamic split-copy is undecided");
        match &plan.kind {
            CopyKind::Dynamic => panic!("dynamic split-copy is undecided"),
            CopyKind::Scalar => copy(source, source_base, dest, scalars, plan.scalar_bytes),
            CopyKind::Reference => copy(source, source_base, dest, refs, plan.reference_bytes),
            CopyKind::Product(fields) => stack.extend(fields.iter().map(|f| {
                (
                    f.layout,
                    source_base + f.source,
                    refs + f.references,
                    scalars + f.scalars,
                )
            })),
            CopyKind::Repeat {
                count,
                source_stride,
                reference_stride,
                scalar_stride,
                element,
            } => stack.extend((0..*count as usize).map(|n| {
                (
                    *element,
                    source_base + n * source_stride,
                    refs + n * reference_stride,
                    scalars + n * scalar_stride,
                )
            })),
            CopyKind::Split {
                reference_source,
                scalars: moves,
            } => {
                copy(
                    source,
                    source_base + reference_source,
                    dest,
                    refs,
                    plan.reference_bytes,
                );
                for transfer in moves {
                    copy(
                        source,
                        source_base + transfer.source.offset,
                        dest,
                        scalars + transfer.destination,
                        transfer.source.bytes,
                    );
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
    match value {
        Value::Scalar(n) => storage.fill(Cell::Scalar(*n)),
        Value::Reference(n) => storage.fill(Cell::Reference(Some(*n))),
        Value::Product(values) => {
            for (field, v) in layout.fields.iter().zip(values) {
                let src = encode(store, field.layout, v);
                copy(&src, 0, &mut storage, field.offset, src.len());
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
                copy(&src, 0, &mut storage, n * source_stride, src.len());
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
    let mut stack = vec![(store.root(), 0usize)];
    let mut reference = |offset: usize| match storage[offset] {
        Cell::Reference(Some(label)) => result.push(label),
        Cell::Reference(None) => {}
        other => panic!("scalar visited as reference: {other:?} at {offset}"),
    };
    while let Some((id, base)) = stack.pop() {
        match &store.get(id).unwrap().trace {
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
                stack.extend(fields.iter().map(|f| (f.layout, base + f.offset)))
            }
            TraceDesc::Repeat {
                count,
                stride,
                element,
            } => stack.extend((0..*count as usize).map(|n| (*element, base + n * stride))),
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
fn dynamic_recipes_compose_but_no_union_inactive_rule_is_selected() {
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
        for root in [4, 5] {
            assert!(matches!(
                LayoutStore::build(nodes.len(), root, domain, |id| Ok(nodes[id])),
                Err(LayoutError::Unsupported)
            ));
        }
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
