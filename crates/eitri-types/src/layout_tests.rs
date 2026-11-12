use super::*;
use ScalarKind::{F32, I8, I64};
use StorageDomain::{ExternC, Frame, Heap};

fn build(
    nodes: &[Shape<'_>],
    root: usize,
    domain: StorageDomain,
) -> Result<LayoutStore, LayoutError> {
    LayoutStore::build(nodes.len(), root, domain, |id| {
        nodes.get(id).copied().ok_or(LayoutError::InvalidId)
    })
}
fn geometry(store: &LayoutStore) -> (usize, usize, Vec<usize>) {
    let layout = store.get(store.root()).unwrap();
    (
        layout.size,
        layout.alignment,
        layout.fields.iter().map(|f| f.offset).collect(),
    )
}
fn references(store: &LayoutStore) -> Vec<usize> {
    let mut result = Vec::new();
    let mut stack = vec![(store.root(), 0usize)];
    while let Some((id, base)) = stack.pop() {
        match &store.get(id).unwrap().trace {
            TraceDesc::None => {}
            TraceDesc::Reference => result.push(base),
            TraceDesc::Product(fields) => {
                stack.extend(fields.iter().map(|f| (f.layout, base + f.offset)))
            }
            TraceDesc::Repeat {
                count,
                stride,
                element,
            } => {
                stack.extend((0..*count).map(|i| (*element, base + i as usize * stride)));
            }
        }
    }
    result.sort_unstable();
    result
}

#[test]
fn padding_nested_products_and_c_abi() {
    #[repr(C)]
    struct Mixed {
        a: i8,
        b: f32,
        c: i64,
    }
    #[repr(C)]
    struct Outer {
        first: u8,
        nested: Mixed,
        last: *const (),
    }
    let nodes = [
        Shape::Scalar(I8),
        Shape::Scalar(F32),
        Shape::Scalar(I64),
        Shape::Product(&[0, 1, 2]),
        Shape::ExternPointer,
        Shape::Product(&[0, 3, 4]),
    ];
    let c = build(&nodes, 3, ExternC).unwrap();
    assert_eq!(
        geometry(&c),
        (
            size_of::<Mixed>(),
            align_of::<Mixed>(),
            vec![
                std::mem::offset_of!(Mixed, a),
                std::mem::offset_of!(Mixed, b),
                std::mem::offset_of!(Mixed, c)
            ]
        )
    );
    let outer = build(&nodes, 5, ExternC).unwrap();
    assert_eq!(
        geometry(&outer),
        (
            size_of::<Outer>(),
            align_of::<Outer>(),
            vec![
                std::mem::offset_of!(Outer, first),
                std::mem::offset_of!(Outer, nested),
                std::mem::offset_of!(Outer, last)
            ]
        )
    );
    assert!(references(&outer).is_empty());
    assert_eq!(geometry(&build(&nodes, 3, Heap).unwrap()), geometry(&c));
    assert_eq!(
        geometry(&build(&nodes, 3, Frame).unwrap()),
        (24, 8, vec![0, 8, 16])
    );
}

#[test]
fn scalar_and_void_layouts_have_no_roots() {
    for (kind, size) in [
        (I8, 1),
        (ScalarKind::U8, 1),
        (ScalarKind::Bool, 1),
        (I64, 8),
        (ScalarKind::U64, 8),
        (F32, 4),
        (ScalarKind::F64, 8),
        (ScalarKind::Char, 4),
    ] {
        let mut types = TypeStore::default();
        let id = types.intern(TypeDesc::Scalar(kind)).unwrap();
        for domain in [Frame, Heap, ExternC] {
            if kind == ScalarKind::Char && domain == ExternC {
                assert!(matches!(
                    LayoutStore::for_type(&types, id, domain),
                    Err(LayoutError::Unsupported)
                ));
                continue;
            }
            let store = LayoutStore::for_type(&types, id, domain).unwrap();
            assert_eq!(
                store.get(store.root()).unwrap().size,
                if domain == Frame { 8 } else { size }
            );
            assert!(references(&store).is_empty());
        }
        let void = types.intern(TypeDesc::Void).unwrap();
        assert_eq!(
            LayoutStore::for_type(&types, void, Frame)
                .unwrap()
                .get(LayoutId(void.0))
                .unwrap()
                .size,
            0
        );
    }
}

#[test]
fn arrays_keep_stride_and_compact_repeat_descriptors() {
    let mut types = TypeStore::default();
    let f = types.intern(TypeDesc::Scalar(F32)).unwrap();
    let a = types
        .intern(TypeDesc::FixedArray {
            count: 3,
            element: f,
        })
        .unwrap();
    let frame = LayoutStore::for_type(&types, a, Frame).unwrap();
    assert_eq!(frame.get(frame.root()).unwrap().array_stride, Some(8));
    assert_eq!(frame.get(frame.root()).unwrap().frame_slots(), 3);
    let optional = types.intern(TypeDesc::Optional(f)).unwrap();
    let result = types
        .intern(TypeDesc::Result { error: f, value: f })
        .unwrap();
    for id in [optional, result] {
        assert!(matches!(
            LayoutStore::for_type(&types, id, Heap),
            Err(LayoutError::Unsupported)
        ));
    }
    let heap = LayoutStore::for_type(&types, a, Heap).unwrap();
    assert_eq!(heap.get(heap.root()).unwrap().array_stride, Some(4));
    assert_eq!(
        geometry(&LayoutStore::for_type(&types, a, Heap).unwrap()).0,
        12
    );
    assert_eq!(
        geometry(&LayoutStore::for_type(&types, a, Frame).unwrap()).0,
        24
    );
    #[repr(C)]
    struct Array {
        a: [f32; 3],
        b: i64,
    }
    let nodes = [
        Shape::Scalar(F32),
        Shape::Array(3, 0),
        Shape::Scalar(I64),
        Shape::Product(&[1, 2]),
    ];
    assert_eq!(
        geometry(&build(&nodes, 3, ExternC).unwrap()),
        (
            size_of::<Array>(),
            align_of::<Array>(),
            vec![0, std::mem::offset_of!(Array, b)]
        )
    );
    let nodes = [
        Shape::ManagedReference(0),
        Shape::Array(0, 0),
        Shape::Array(3, 0),
        Shape::Array(u32::MAX, 0),
    ];
    let zero = build(&nodes, 1, Heap).unwrap();
    assert_eq!(zero.get(zero.root()).unwrap().size, 0);
    assert_eq!(
        zero.get(zero.root()).unwrap().alignment,
        align_of::<*const ()>()
    );
    assert!(references(&zero).is_empty());
    let three = build(&nodes, 2, Heap).unwrap();
    let stride = size_of::<*const ()>();
    assert_eq!(references(&three), vec![0, stride, stride * 2]);
    if usize::BITS == 64 {
        let huge = build(&nodes, 3, Heap).unwrap();
        assert!(matches!(
            huge.get(huge.root()).unwrap().trace,
            TraceDesc::Repeat {
                count: u32::MAX,
                ..
            }
        ));
        assert_eq!(huge.layouts.iter().filter(|l| l.is_some()).count(), 2);
    }
}

#[test]
fn only_managed_leaves_are_traced_and_reference_edges_end_cycles() {
    let nodes = [
        Shape::Scalar(I8),
        Shape::ManagedReference(5),
        Shape::ExternPointer,
        Shape::HostIdentity {
            size: 8,
            alignment: 4,
        },
        Shape::Product(&[0, 1]),
        Shape::Product(&[4, 2, 3]),
        Shape::Array(2, 5),
    ];
    for domain in [Heap, Frame] {
        let store = build(&nodes, 6, domain).unwrap();
        let inner = store.get(LayoutId(4)).unwrap();
        let outer = store.get(LayoutId(5)).unwrap();
        let offset = inner.fields[1].offset;
        assert_eq!(references(&store), vec![offset, outer.size + offset]);
    }
    assert!(matches!(
        build(&nodes, 6, ExternC),
        Err(LayoutError::Unsupported)
    ));
    assert!(matches!(
        build(&nodes, 3, ExternC),
        Err(LayoutError::Unsupported)
    ));
}

#[test]
fn invalid_cycles_alignment_and_overflow_reject_without_recursion() {
    for nodes in [
        vec![Shape::Product(&[0])],
        vec![Shape::Array(0, 0)],
        vec![Shape::Product(&[1]), Shape::Product(&[0])],
    ] {
        assert!(matches!(
            build(&nodes, 0, Heap),
            Err(LayoutError::InlineCycle)
        ));
    }
    assert!(matches!(
        build(&[Shape::Array(1, 9)], 0, Heap),
        Err(LayoutError::InvalidId)
    ));
    assert!(matches!(
        build(&[Shape::ManagedReference(9)], 0, Heap),
        Err(LayoutError::InvalidId)
    ));
    assert!(matches!(build(&[], 0, Heap), Err(LayoutError::InvalidId)));
    for alignment in [0, 3] {
        assert!(matches!(
            build(&[Shape::HostIdentity { size: 8, alignment }], 0, Heap),
            Err(LayoutError::InvalidAlignment)
        ));
    }
    let nodes = [
        Shape::HostIdentity {
            size: isize::MAX as usize - 7,
            alignment: 8,
        },
        Shape::Array(3, 0),
        Shape::Product(&[0, 0]),
        Shape::Product(&[0, 0, 0]),
    ];
    for root in [1, 2, 3] {
        assert!(matches!(
            build(&nodes, root, Heap),
            Err(LayoutError::Overflow)
        ));
    }
    assert_eq!(aligned(usize::MAX, 8), Err(LayoutError::Overflow));
    let edges: Vec<_> = (0..20_000).map(|i| [i + 1]).collect();
    let mut nodes: Vec<_> = edges.iter().map(|e| Shape::Product(e)).collect();
    nodes.push(Shape::Scalar(I8));
    assert_eq!(geometry(&build(&nodes, 0, Heap).unwrap()).0, 1);
}

#[test]
fn generated_offsets_match_independent_byte_calculator() {
    // Independent small-shape oracle pads one byte at a time, not via aligned().
    fn oracle(nodes: &[Shape<'_>], id: usize) -> (usize, usize, Vec<usize>) {
        match nodes[id] {
            Shape::Scalar(I8) => (1, 1, vec![]),
            Shape::Scalar(F32) => (4, align_of::<f32>(), vec![]),
            Shape::Scalar(I64) => (8, align_of::<i64>(), vec![]),
            Shape::Array(n, child) => {
                let (size, align, _) = oracle(nodes, child);
                (size * n as usize, align, vec![])
            }
            Shape::Product(fields) => {
                let (mut size, mut align, mut offsets) = (0usize, 1usize, vec![]);
                for &child in fields {
                    let (bytes, a, _) = oracle(nodes, child);
                    while !size.is_multiple_of(a) {
                        size += 1;
                    }
                    offsets.push(size);
                    size += bytes;
                    align = align.max(a);
                }
                while !size.is_multiple_of(align) {
                    size += 1;
                }
                (size, align, offsets)
            }
            _ => unreachable!(),
        }
    }
    let mut seed = 17u64;
    for _ in 0..512 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let fields: Vec<_> = (0..8).map(|n| ((seed >> (n * 3)) % 6) as usize).collect();
        let nodes = [
            Shape::Scalar(I8),
            Shape::Scalar(F32),
            Shape::Scalar(I64),
            Shape::Array((seed % 5) as u32, 0),
            Shape::Array(((seed >> 8) % 5) as u32, 1),
            Shape::Product(&[0, 1, 2]),
            Shape::Product(&fields),
        ];
        for domain in [Heap, ExternC] {
            assert_eq!(
                geometry(&build(&nodes, 6, domain).unwrap()),
                oracle(&nodes, 6),
                "seed {seed}"
            );
        }
    }
}
