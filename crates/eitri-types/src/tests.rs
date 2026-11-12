use super::*;

#[test]
fn canonical_constructors_and_optional_collapse() {
    let mut store = TypeStore::default();
    let scalar = store.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
    assert_eq!(store.intern(TypeDesc::Scalar(ScalarKind::I64)), Ok(scalar));
    let optional = store.intern(TypeDesc::Optional(scalar)).unwrap();
    assert_eq!(store.intern(TypeDesc::Optional(optional)), Ok(optional));
    let array = TypeDesc::FixedArray {
        count: 4,
        element: optional,
    };
    let a = store.intern(array).unwrap();
    assert_eq!(store.intern(array), Ok(a));
    let result = TypeDesc::Result {
        error: scalar,
        value: a,
    };
    let r = store.intern(result).unwrap();
    assert_eq!(store.intern(result), Ok(r));
    let signature = store.signature(&[a], r).unwrap();
    // Independent expected prefix encoding: no TypeIds occur here.
    assert_eq!(
        &*signature.encoding,
        &[
            1, 0, 0, 0, 10, 4, 0, 0, 0, 9, 1, 11, 1, 10, 4, 0, 0, 0, 9, 1
        ]
    );
}

#[test]
fn structural_equality_ignores_local_ids_and_owns_its_data() {
    fn build(reverse: bool) -> PortableSignature {
        let mut store = TypeStore::default();
        if reverse {
            store.intern(TypeDesc::Void).unwrap();
        }
        let scalar = store.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
        assert_eq!(scalar.0, u32::from(reverse));
        let optional = store.intern(TypeDesc::Optional(scalar)).unwrap();
        let array = store
            .intern(TypeDesc::FixedArray {
                count: 7,
                element: optional,
            })
            .unwrap();
        store.signature(&[array, scalar], optional).unwrap()
    }
    assert_eq!(build(false), build(true)); // Both source stores have died.
    let mut a = TypeStore::default();
    let mut b = TypeStore::default();
    let i = a.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
    let u = b.intern(TypeDesc::Scalar(ScalarKind::U64)).unwrap();
    assert_eq!(i.0, u.0); // Same local index, different meaning.
    assert_ne!(a.signature(&[], i).unwrap(), b.signature(&[], u).unwrap());
}

#[test]
fn signature_order_counts_constructors_and_scalar_kinds_differ() {
    let mut s = TypeStore::default();
    let i = s.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
    let u = s.intern(TypeDesc::Scalar(ScalarKind::U64)).unwrap();
    let void = s.intern(TypeDesc::Void).unwrap();
    let optional = s.intern(TypeDesc::Optional(i)).unwrap();
    let one = s
        .intern(TypeDesc::FixedArray {
            count: 1,
            element: i,
        })
        .unwrap();
    let two = s
        .intern(TypeDesc::FixedArray {
            count: 2,
            element: i,
        })
        .unwrap();
    let result = s.intern(TypeDesc::Result { error: i, value: u }).unwrap();
    let swapped = s.intern(TypeDesc::Result { error: u, value: i }).unwrap();
    let kinds = [i, u, void, optional, one, two, result, swapped];
    for (n, &a) in kinds.iter().enumerate() {
        for &b in &kinds[n + 1..] {
            assert_ne!(s.signature(&[], a).unwrap(), s.signature(&[], b).unwrap());
        }
    }
    assert_ne!(s.signature(&[i, u], void), s.signature(&[u, i], void));
    assert_ne!(s.signature(&[i], void), s.signature(&[i, i], void));
    for kind in [
        ScalarKind::I8,
        ScalarKind::U8,
        ScalarKind::F32,
        ScalarKind::F64,
        ScalarKind::Char,
        ScalarKind::Bool,
    ] {
        let other = s.intern(TypeDesc::Scalar(kind)).unwrap();
        assert_ne!(s.signature(&[], i), s.signature(&[], other));
    }
}

#[test]
fn malformed_ids_and_expansion_limits_leave_store_usable() {
    let mut s = TypeStore::default();
    let bad = TypeId(u32::MAX);
    for desc in [
        TypeDesc::Optional(bad),
        TypeDesc::FixedArray {
            count: 0,
            element: bad,
        },
        TypeDesc::Result {
            error: bad,
            value: bad,
        },
    ] {
        assert_eq!(s.intern(desc), Err(TypeError::InvalidId));
    }
    let i = s.intern(TypeDesc::Scalar(ScalarKind::I64)).unwrap();
    assert_eq!(s.signature(&[bad], i), Err(TypeError::InvalidId));
    assert_eq!(s.signature(&[], bad), Err(TypeError::InvalidId));
    assert_eq!(
        add_size(usize::MAX, 1, usize::MAX),
        Err(TypeError::TooLarge)
    );
    assert_eq!(
        add_size(MAX_TYPE_BYTES, 1, MAX_TYPE_BYTES),
        Err(TypeError::TooLarge)
    );
    assert_eq!(
        add_size(4, MAX_SIGNATURE_BYTES, MAX_SIGNATURE_BYTES),
        Err(TypeError::TooLarge)
    );
    // Count is encoded, never multiplied into the descriptor size.
    let huge = s
        .intern(TypeDesc::FixedArray {
            count: u32::MAX,
            element: i,
        })
        .unwrap();
    assert_eq!(s.signature(&[], huge).unwrap().heap_bytes(), 10);
    let mut child = i;
    while let Ok(next) = s.intern(TypeDesc::Result {
        error: child,
        value: child,
    }) {
        child = next;
    }
    let before = s.nodes.len();
    assert_eq!(
        s.intern(TypeDesc::Result {
            error: child,
            value: child
        }),
        Err(TypeError::TooLarge)
    );
    assert_eq!(s.nodes.len(), before);
    assert_eq!(s.intern(TypeDesc::Scalar(ScalarKind::I64)), Ok(i));
}
