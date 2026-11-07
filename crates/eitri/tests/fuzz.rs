//! Seeded mutation fuzzing (T012) of the compiler and verifier boundaries,
//! through the entry points in `support/fuzz.rs`. Inputs are mutations of
//! the fixture corpus: source bytes for the compiler, code words of the
//! compiled corpus for the verifier and both interpreters.
//!
//! Deterministic: case `n` of each target uses seed `n`. The default run is
//! quick; `EITRI_FUZZ_CASES=<count>` runs more. A failure names its seed.

mod support;

use std::fs;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::thread;

use support::fuzz;

const DEFAULT_CASES: u64 = 2_000;

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Tokens worth inserting: every keyword and operator of the slice, its
/// neighbors, boundary literals, and lexically awkward text.
const DICTIONARY: &[&str] = &[
    "fn",
    "let",
    "var",
    "while",
    "if",
    "else",
    "return",
    "import",
    "from",
    "true",
    "false",
    "i64",
    "bool",
    "void",
    "u8",
    "struct",
    "for",
    "break",
    "&+",
    "&-",
    "&*",
    "+",
    "-",
    "*",
    "/",
    "%",
    "+=",
    "-=",
    "*=",
    "/=",
    "%=",
    "&+=",
    "&&",
    "||",
    "!",
    "==",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "<<",
    "=",
    "{",
    "}",
    "(",
    ")",
    "[",
    "]",
    ",",
    ":",
    ";",
    ".",
    "->",
    "\n",
    " ",
    "\t",
    "//",
    "/*",
    "*/",
    "\"",
    "'",
    "@",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775808",
    "0x",
    "1_000",
    "0",
    "test.emit(",
    "test.check(",
    "test.fail()",
    "x",
    "é",
    "\u{0}",
];

fn corpus() -> Vec<Vec<u8>> {
    fn collect(directory: &Path, paths: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect(&path, paths);
            } else if path.extension().is_some_and(|e| e == "eit") {
                paths.push(path);
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut paths = Vec::new();
    collect(&root.join("tests/fixtures"), &mut paths);
    collect(&root.join("benches/fixtures"), &mut paths);
    paths.sort();
    let corpus: Vec<_> = paths.iter().map(|p| fs::read(p).unwrap()).collect();
    assert!(corpus.len() > 100, "the corpus was found");
    corpus
}

/// Edits of a corpus entry: half the time one edit of a program that
/// compiles, so many mutants still run and are compared; otherwise one to
/// four edits of any entry.
fn mutate_source(
    rng: &mut Rng,
    corpus: &[Vec<u8>],
    compiling: &[usize],
    dictionary: &[&str],
) -> Vec<u8> {
    let (mut data, edits) = if rng.below(2) == 0 {
        (corpus[compiling[rng.below(compiling.len())]].clone(), 1)
    } else {
        (corpus[rng.below(corpus.len())].clone(), 1 + rng.below(4))
    };
    for _ in 0..edits {
        let at = rng.below(data.len() + 1);
        match rng.below(6) {
            0 if !data.is_empty() => {
                let last = data.len() - 1;
                data[at.min(last)] = rng.next() as u8;
            }
            1 => {
                let end = (at + rng.below(16)).min(data.len());
                data.drain(at..end);
            }
            2 => {
                let end = (at + rng.below(32)).min(data.len());
                let copy = data[at..end].to_vec();
                let to = rng.below(data.len() + 1);
                data.splice(to..to, copy);
            }
            3 => {
                // Splice with another entry.
                let other = &corpus[rng.below(corpus.len())];
                let from = rng.below(other.len() + 1);
                data.truncate(at);
                data.extend_from_slice(&other[from..]);
            }
            _ => {
                let token = dictionary[rng.below(dictionary.len())].as_bytes();
                data.splice(at..at, token.iter().copied());
            }
        }
    }
    data
}

/// Run `case` for every seed on all cores; return the cases accepted past
/// the front door, or the first failure with its seed.
fn drive(cases: u64, case: impl Fn(u64) -> bool + Sync) -> u64 {
    let workers = thread::available_parallelism().map_or(1, |n| n.get() as u64);
    let results: Vec<Result<u64, String>> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let case = &case;
                scope.spawn(move || {
                    let mut accepted = 0;
                    for seed in (worker..cases).step_by(workers as usize) {
                        match panic::catch_unwind(AssertUnwindSafe(|| case(seed))) {
                            Ok(true) => accepted += 1,
                            Ok(false) => {}
                            Err(_) => {
                                return Err(format!("seed {seed} failed; see its panic above"));
                            }
                        }
                    }
                    Ok(accepted)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut accepted = 0;
    for result in results {
        accepted += result.unwrap_or_else(|failure| panic!("{failure}"));
    }
    accepted
}

fn cases() -> u64 {
    std::env::var("EITRI_FUZZ_CASES")
        .map(|count| count.parse().expect("EITRI_FUZZ_CASES is an integer"))
        .unwrap_or(DEFAULT_CASES)
}

#[test]
fn mutated_sources() {
    let corpus = corpus();
    let compiling: Vec<usize> = (0..corpus.len())
        .filter(|&i| fuzz::compile(std::str::from_utf8(&corpus[i]).unwrap()).is_some())
        .collect();
    let cases = cases();
    let compiled = drive(cases, |seed| {
        let data = mutate_source(&mut Rng(seed), &corpus, &compiling, DICTIONARY);
        let report = || format!("seed {seed}, input {:?}", String::from_utf8_lossy(&data));
        match panic::catch_unwind(AssertUnwindSafe(|| fuzz::source(&data))) {
            Ok(compiled) => compiled,
            Err(payload) => {
                eprintln!("{}", report());
                panic::resume_unwind(payload)
            }
        }
    });
    eprintln!("{cases} mutated sources, {compiled} compiled and agreed");
}

/// `.eits` tokens worth inserting: directives, mnemonics, slots at the
/// `WIDE` boundary, labels, names, and literals at the load-form edges.
const ASSEMBLY: &[&str] = &[
    "define",
    "declare",
    "frame(",
    ")",
    "(",
    "{",
    "}",
    ",",
    ":",
    "=",
    "\n",
    "%r0",
    "%r1",
    "%r255",
    "%r256",
    "%r65535",
    "%r65536",
    "%r70000",
    "%r4294967295",
    "label %bb0",
    "label %bb1",
    "%bb9",
    "bb1:",
    "@<module>",
    "@f0",
    "@test.emit",
    "@test.reenter",
    "@\"",
    "i64",
    "bool",
    "void",
    "true",
    "ldi",
    "ldk",
    "ldk_x",
    "mov",
    "add.chk.s",
    "div.s",
    "cbi.lt",
    "cbi.gt",
    "cb.le",
    "tb.true",
    "j",
    "j_x",
    "wide",
    "ret",
    "call.script",
    "call.host",
    "32767",
    "32768",
    "-129",
    "127",
    "9223372036854775807",
    ";",
];

#[test]
fn mutated_assembly() {
    // The printed text of every program the corpus compiles.
    let texts: Vec<Vec<u8>> = corpus()
        .iter()
        .filter_map(|data| fuzz::print(std::str::from_utf8(data).ok()?))
        .map(String::into_bytes)
        .collect();
    assert!(texts.len() > 30, "the corpus compiles");
    let all: Vec<usize> = (0..texts.len()).collect();
    let cases = cases();
    let assembled = drive(cases, |seed| {
        let data = mutate_source(&mut Rng(seed), &texts, &all, ASSEMBLY);
        let report = || format!("seed {seed}, input {:?}", String::from_utf8_lossy(&data));
        match panic::catch_unwind(AssertUnwindSafe(|| fuzz::assembly(&data))) {
            Ok(assembled) => assembled,
            Err(payload) => {
                eprintln!("{}", report());
                panic::resume_unwind(payload)
            }
        }
    });
    eprintln!("{cases} mutated assembly texts, {assembled} assembled, verified, and agreed");
}

#[test]
fn mutated_bytecode() {
    let programs: Vec<fuzz::Compiled> = corpus()
        .iter()
        .filter_map(|data| fuzz::compile(std::str::from_utf8(data).ok()?))
        .collect();
    assert!(programs.len() > 30, "the corpus compiles");
    let cases = cases();
    let ran = drive(cases, |seed| {
        let mut rng = Rng(seed);
        let compiled = &programs[rng.below(programs.len())];
        let mut functions = compiled.program.functions.clone();
        let index = rng.below(functions.len());
        let function = &mut functions.as_mut_slice()[index];
        let mut code = function.code.to_vec();
        for _ in 0..1 + rng.below(3) {
            let at = rng.below(code.len());
            match rng.below(6) {
                // Operand fields: slots, immediates, displacements.
                0 | 1 => code[at] ^= 1 << (8 + rng.below(24)),
                2 => code[at] = (code[at] & !0xff) | rng.below(256) as u32,
                3 => code[at] = rng.next() as u32,
                4 => {
                    let other = rng.below(code.len());
                    code.swap(at, other);
                }
                _ => code.truncate(at.max(1)),
            }
        }
        function.code = code.into_boxed_slice();
        if rng.below(8) == 0 {
            function.frame_size = function.frame_size.saturating_sub(1);
        }
        fuzz::bytecode(&functions, &compiled.hosts)
    });
    eprintln!("{cases} mutated programs, {ran} verified and ran on both interpreters");
}
