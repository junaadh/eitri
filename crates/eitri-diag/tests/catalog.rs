//! The code catalog and the specification's code table must agree, so the
//! documentation stays true as codes are added.

use eitri_diag::Code;

#[test]
fn catalog_matches_the_first_slice_table() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/implementation/first-slice.md"
    );
    let doc = std::fs::read_to_string(path).unwrap();
    let section = doc
        .split("## Diagnostic codes")
        .nth(1)
        .expect("the chapter has a diagnostic code table");
    let documented: Vec<(String, String)> = section
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("| `E")?;
            let (number, meaning) = rest.split_once("` | ")?;
            Some((format!("E{number}"), meaning.strip_suffix(" |")?.to_owned()))
        })
        .collect();
    let catalog: Vec<(String, String)> = Code::ALL
        .iter()
        .map(|code| (code.to_string(), code.summary().to_owned()))
        .collect();
    assert_eq!(catalog, documented);
}
