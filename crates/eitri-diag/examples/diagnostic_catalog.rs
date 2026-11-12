//! Generate/check the published table from the existing Code::ALL catalog.

#[path = "../tools/catalog.rs"]
mod catalog;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 1 || !matches!(args[0].as_str(), "--check" | "--write") {
        return Err("usage: diagnostic_catalog --check|--write".into());
    }
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/implementation/first-slice.md"
    );
    let old = std::fs::read_to_string(path)?;
    let new = catalog::replace(&old)?;
    if args[0] == "--write" {
        if old != new {
            std::fs::write(path, new)?;
        }
        println!("diagnostic catalog generated from Code::ALL");
    } else if old != new {
        return Err("diagnostic catalog is stale; run with --write and review the diff".into());
    } else {
        println!(
            "diagnostic catalog is current ({} codes)",
            eitri_diag::Code::ALL.len()
        );
    }
    Ok(())
}
