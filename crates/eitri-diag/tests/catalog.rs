//! Generation is explicit; ordinary tests only check for drift.

#[path = "../tools/catalog.rs"]
mod catalog;

#[test]
fn generated_catalog_matches_the_first_slice() {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/implementation/first-slice.md"
    ))
    .unwrap();
    assert_eq!(
        doc,
        catalog::replace(&doc).unwrap(),
        "run cargo run --locked -p eitri-diag --example diagnostic_catalog -- --write and review"
    );
}

#[test]
fn generation_preserves_surrounding_text_and_requires_one_marker_pair() {
    let document = format!(
        "before\n{}\nstale\n{}\nafter\n",
        catalog::START,
        catalog::END
    );
    let generated = catalog::replace(&document).unwrap();
    assert_eq!(generated, format!("before\n{}\nafter\n", catalog::table()));
    assert_eq!(catalog::replace(&generated).unwrap(), generated);
    for invalid in [
        "".to_owned(),
        catalog::START.to_owned(),
        format!("{}{}", catalog::END, catalog::START),
        format!("{document}{document}"),
    ] {
        assert!(catalog::replace(&invalid).is_err(), "{invalid}");
    }
}
