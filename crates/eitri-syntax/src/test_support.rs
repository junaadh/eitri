//! Inputs shared by the differential tests: the fixture and example corpus,
//! seeded mutations of it, and stress fragments.

/// A small deterministic PRNG (xorshift64*), so failures reproduce from the seed.
pub(crate) struct Rng(pub(crate) u64);

impl Rng {
    pub(crate) fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub(crate) fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Fragments that stress lexer states and boundaries.
pub(crate) const FRAGMENTS: &[&str] = &[
    "\"",
    "'",
    "${",
    "}",
    "{",
    "(",
    ")",
    "[",
    "]",
    "?[",
    "/*",
    "*/",
    "//",
    "\n",
    "\r\n",
    "\t",
    "\\",
    "\\n",
    "\\u{2603}",
    "0x",
    "0b1",
    "0o7",
    "1.5e",
    "1e+3",
    "9_9",
    "..",
    "..=",
    ".",
    "@",
    "@extern",
    "try?",
    "try",
    ">>=",
    ">>",
    "&+=",
    "&-",
    "<<=",
    "r\"",
    "\"\"\"",
    "\"\"\"\n",
    "💥",
    "ދި",
    "ï",
    "_",
    "_x",
    "let",
    "fn",
    "while",
    "x1",
    " ",
    "  ",
    "\u{0}",
    "\u{7}",
    "$",
    "#",
    "`",
    "=",
    "==",
    "=>",
    "->",
    "?",
    "??",
    "?.",
    "!",
    "!=",
    ",",
    ";",
    ":",
];

pub(crate) fn corpus() -> Vec<String> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let mut files = Vec::new();
    let mut stack = vec![
        std::path::PathBuf::from(format!("{root}/docs/examples/editor-config")),
        std::path::PathBuf::from(format!("{root}/crates/eitri/tests/fixtures")),
    ];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "eit") {
                files.push(std::fs::read_to_string(path).unwrap());
            }
        }
    }
    files.sort();
    assert!(
        files.len() > 90,
        "corpus discovery found {} files",
        files.len()
    );
    files
}

/// Replace, delete, duplicate, or insert at random character boundaries.
pub(crate) fn mutate(rng: &mut Rng, text: &str) -> String {
    let mut out = text.to_owned();
    for _ in 0..1 + rng.below(4) {
        // Recompute every round: an equal length does not mean equal
        // character boundaries once multi-byte characters have moved.
        let bounds: Vec<usize> = (0..=out.len())
            .filter(|&i| out.is_char_boundary(i))
            .collect();
        let a = bounds[rng.below(bounds.len())];
        let b = bounds[rng.below(bounds.len())];
        let (a, b) = (a.min(b), a.max(b).min(a + 40));
        let b = (b..=out.len()).find(|&i| out.is_char_boundary(i)).unwrap();
        let fragment = FRAGMENTS[rng.below(FRAGMENTS.len())];
        out = match rng.below(4) {
            0 => format!("{}{}{}", &out[..a], fragment, &out[b..]),
            1 => format!("{}{}", &out[..a], &out[b..]),
            2 => format!("{}{}{}", &out[..b], &out[a..b], &out[b..]),
            _ => format!("{}{}{}", &out[..a], fragment, &out[a..]),
        };
    }
    out
}
