//! Differential testing (T012): seeded random programs of the first slice,
//! well typed by construction, run by both interpreters and the
//! program-level oracle, which must agree on host effects, the result, and
//! any fault with its trace.
//!
//! Deterministic: case `n` is generated from seed `n`. The default run is
//! quick; `EITRI_DIFF_CASES=<count>` runs more, and `EITRI_DIFF_SEED=<n>`
//! reruns one case and prints its source.

mod support;

use std::{fmt::Write, thread};

use support::Setup;

const DEFAULT_CASES: u64 = 300;

#[test]
fn interpreters_agree_with_the_oracle() {
    if let Ok(seed) = std::env::var("EITRI_DIFF_SEED") {
        let seed = seed.parse().expect("EITRI_DIFF_SEED is an integer");
        let (source, setup) = generate(seed);
        eprintln!("{source}");
        let outcome = check(seed, &source, &setup).unwrap_or_else(|failure| panic!("{failure}"));
        eprintln!("{outcome}");
        return;
    }
    let cases = std::env::var("EITRI_DIFF_CASES")
        .map(|count| count.parse().expect("EITRI_DIFF_CASES is an integer"))
        .unwrap_or(DEFAULT_CASES);
    let workers = thread::available_parallelism().map_or(1, |n| n.get() as u64);
    let results: Vec<Result<String, String>> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                scope.spawn(move || {
                    (worker..cases)
                        .step_by(workers as usize)
                        .map(|seed| {
                            let (source, setup) = generate(seed);
                            check(seed, &source, &setup)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("a worker does not panic"))
            .collect()
    });
    let failures: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    if let Some(first) = failures.first() {
        panic!("{} of {cases} cases disagree\n{first}", failures.len());
    }
    // How the cases ended, so a generator that only produces early faults
    // is visible.
    let mut ends: Vec<(String, usize)> = Vec::new();
    for outcome in results.iter().flatten() {
        let last = outcome
            .lines()
            .find(|l| !l.starts_with("effect: "))
            .unwrap_or("");
        match ends.iter_mut().find(|(end, _)| end == last) {
            Some((_, count)) => *count += 1,
            None => ends.push((last.to_owned(), 1)),
        }
    }
    let effects: usize = results
        .iter()
        .flatten()
        .map(|outcome| {
            outcome
                .lines()
                .filter(|l| l.starts_with("effect: "))
                .count()
        })
        .sum();
    ends.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    eprintln!("{cases} cases agree, {effects} host effects compared");
    for (end, count) in ends {
        eprintln!("  {count:>6}  {end}");
    }
}

/// The agreed outcome, or why the case failed.
fn check(seed: u64, source: &str, setup: &Setup) -> Result<String, String> {
    let outcome = support::outcome(source, setup)
        .map_err(|disagreement| format!("seed {seed}: {disagreement}\n{source}"))?;
    if outcome.starts_with("error[") || outcome.starts_with("bug: ") {
        return Err(format!(
            "seed {seed}: the generator produced a rejected program\n{outcome}{source}"
        ));
    }
    support::eits::round_trip(source)
        .map_err(|failure| format!("seed {seed}: {failure}\n{source}"))?;
    Ok(outcome)
}

/// splitmix64: small, seedable, and good enough to pick productions.
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
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    Int,
    Bool,
    Void,
}

struct Function {
    params: Vec<Ty>,
    ret: Ty,
}

#[derive(Clone, Copy)]
struct Var {
    id: usize,
    ty: Ty,
    mutable: bool,
}

/// Literals at the representation boundaries: the `ALUI`/`CBI` 8-bit
/// immediates, the `LDI` 16-bit immediate, and the ends of `i64`. Small
/// values dominate so programs run past their first operations.
const LITERALS: &[i64] = &[
    0,
    1,
    2,
    3,
    5,
    -1,
    -2,
    7,
    127,
    128,
    -128,
    -129,
    255,
    32_767,
    32_768,
    -32_768,
    -32_769,
    i64::MAX,
    i64::MIN,
    i64::MAX - 1,
    i64::MIN + 1,
];

struct Gen {
    rng: Rng,
    out: String,
    functions: Vec<Function>,
    /// Visible variables, innermost last.
    scope: Vec<Var>,
    next_var: usize,
    /// The function being generated: it may call only later functions, so
    /// calls are acyclic and every program terminates or exhausts a budget.
    current: Option<usize>,
    /// The function named `callback`, which `test.reenter` runs, if any.
    callback: Option<usize>,
    indent: usize,
}

fn generate(seed: u64) -> (String, Setup) {
    let mut g = Gen {
        rng: Rng(seed),
        out: String::new(),
        functions: Vec::new(),
        scope: Vec::new(),
        next_var: 0,
        current: None,
        callback: None,
        indent: 0,
    };
    let mut count = g.rng.below(5);
    for _ in 0..count {
        let params = (0..g.rng.below(4))
            .map(|_| g.rng.pick(&[Ty::Int, Ty::Int, Ty::Bool]))
            .collect();
        let ret = g.rng.pick(&[Ty::Int, Ty::Int, Ty::Bool, Ty::Void]);
        g.functions.push(Function { params, ret });
    }
    // Last, so it calls no other function; it may reenter itself, which
    // reaches the nesting and budget limits.
    if g.rng.chance(40) {
        g.functions.push(Function {
            params: vec![Ty::Int],
            ret: Ty::Int,
        });
        g.callback = Some(count);
        count += 1;
    }
    g.out += "import test from \"test\"\n";
    for f in 0..count {
        g.function(f);
    }
    g.current = None;
    g.scope.clear();
    for _ in 0..1 + g.rng.below(8) {
        g.stmt(3);
    }
    let setup = Setup {
        test_host: true,
        // Often small enough to run out, so budget faults are compared too.
        budget: g.rng.chance(30).then(|| g.rng.below(60) as u64),
        depth: None,
    };
    (g.out, setup)
}

impl Gen {
    fn line(&mut self, text: &str) {
        for _ in 0..self.indent {
            self.out += "    ";
        }
        self.out += text;
        self.out += "\n";
    }

    fn name(&self, f: usize) -> String {
        if self.callback == Some(f) {
            "callback".to_owned()
        } else {
            format!("f{f}")
        }
    }

    fn fresh(&mut self, ty: Ty, mutable: bool) -> Var {
        let var = Var {
            id: self.next_var,
            ty,
            mutable,
        };
        self.next_var += 1;
        var
    }

    fn function(&mut self, f: usize) {
        self.current = Some(f);
        self.scope.clear();
        let mut params = String::new();
        for (i, &ty) in self.functions[f].params.clone().iter().enumerate() {
            let var = self.fresh(ty, false);
            if i > 0 {
                params += ", ";
            }
            write!(params, "v{}: {}", var.id, type_name(ty)).unwrap();
            self.scope.push(var);
        }
        let ret = self.functions[f].ret;
        let name = self.name(f);
        self.line(&format!("fn {name}({params}) -> {} {{", type_name(ret)));
        self.indent += 1;
        for _ in 0..self.rng.below(5) {
            self.stmt(2);
        }
        if ret != Ty::Void {
            let value = self.expr(ret, 3);
            self.line(&format!("return {value}"));
        } else if self.rng.chance(30) {
            self.line("return");
        }
        self.indent -= 1;
        self.line("}");
    }

    /// One statement; `depth` bounds nested blocks.
    fn stmt(&mut self, depth: u32) {
        let choice = self.rng.below(if depth == 0 { 4 } else { 7 });
        match choice {
            0 | 1 => {
                let ty = self.rng.pick(&[Ty::Int, Ty::Int, Ty::Bool]);
                let mutable = self.rng.chance(50);
                let value = if ty != Ty::Void && depth > 0 && self.rng.chance(15) {
                    self.if_value(ty, depth - 1)
                } else {
                    self.expr(ty, 3)
                };
                let var = self.fresh(ty, mutable);
                let keyword = if mutable { "var" } else { "let" };
                self.line(&format!(
                    "{keyword} v{}: {} = {value}",
                    var.id,
                    type_name(ty)
                ));
                self.scope.push(var);
            }
            2 => {
                let mutable: Vec<Var> = self.scope.iter().copied().filter(|v| v.mutable).collect();
                if mutable.is_empty() {
                    return self.emit();
                }
                let var = self.rng.pick(&mutable);
                let op = match var.ty {
                    Ty::Int => self
                        .rng
                        .pick(&["=", "+=", "-=", "*=", "/=", "%=", "&+=", "&-=", "&*="]),
                    _ => "=",
                };
                let value = self.expr(var.ty, 3);
                self.line(&format!("v{} {op} {value}", var.id));
            }
            3 => self.emit(),
            4 => {
                let condition = self.expr(Ty::Bool, 2);
                self.line(&format!("if {condition} {{"));
                self.block(depth - 1);
                if self.rng.chance(50) {
                    self.line("} else {");
                    self.block(depth - 1);
                }
                self.line("}");
            }
            5 => {
                // A counted loop, sometimes with an extra exit condition.
                let counter = self.fresh(Ty::Int, true);
                let limit = self.rng.below(5);
                self.line(&format!("var v{} = 0", counter.id));
                let extra = if self.rng.chance(30) {
                    format!(" && {}", self.expr(Ty::Bool, 1))
                } else {
                    String::new()
                };
                self.line(&format!("while v{} < {limit}{extra} {{", counter.id));
                self.indent += 1;
                self.line(&format!("v{} += 1", counter.id));
                self.indent -= 1;
                self.block(depth - 1);
                self.line("}");
            }
            _ => {
                let callee = self.callee(Ty::Void);
                match callee {
                    Some(call) => self.line(&call),
                    None => self.emit(),
                }
            }
        }
    }

    fn block(&mut self, depth: u32) {
        let mark = self.scope.len();
        self.indent += 1;
        for _ in 0..self.rng.below(4) {
            self.stmt(depth);
        }
        self.indent -= 1;
        self.scope.truncate(mark);
    }

    fn emit(&mut self) {
        let value = self.expr(Ty::Int, 3);
        self.line(&format!("test.emit({value})"));
    }

    /// `if … { … } else …` producing a value, written over several lines.
    fn if_value(&mut self, ty: Ty, depth: u32) -> String {
        let condition = self.expr(Ty::Bool, 2);
        let then = self.expr(ty, 2);
        let mut text = format!("if {condition} {{ {then} }}");
        if self.rng.chance(30) {
            let condition = self.expr(Ty::Bool, 1);
            let value = self.expr(ty, 1);
            write!(text, " else if {condition} {{ {value} }}").unwrap();
        }
        let otherwise = self.expr(ty, depth.min(2));
        write!(text, " else {{ {otherwise} }}").unwrap();
        text
    }

    /// A call to a later function returning `ty`, if there is one.
    fn callee(&mut self, ty: Ty) -> Option<String> {
        let first = self.current.map_or(0, |f| f + 1);
        let candidates: Vec<usize> = (first..self.functions.len())
            .filter(|&f| self.functions[f].ret == ty)
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let f = self.rng.pick(&candidates);
        let params = self.functions[f].params.clone();
        let args: Vec<String> = params.iter().map(|&p| self.expr(p, 1)).collect();
        Some(format!("{}({})", self.name(f), args.join(", ")))
    }

    fn literal(&mut self) -> String {
        match self.rng.pick(LITERALS) {
            value if value < 0 => format!("({value})"),
            value => value.to_string(),
        }
    }

    fn var(&mut self, ty: Ty) -> Option<String> {
        let vars: Vec<Var> = self.scope.iter().copied().filter(|v| v.ty == ty).collect();
        (!vars.is_empty()).then(|| format!("v{}", self.rng.pick(&vars).id))
    }

    /// An expression of type `ty`, at most `depth` operators deep.
    fn expr(&mut self, ty: Ty, depth: u32) -> String {
        let leaf = depth == 0 || self.rng.chance(30);
        match ty {
            Ty::Int if leaf => match self.rng.below(3) {
                0 => self.literal(),
                _ => self.var(Ty::Int).unwrap_or_else(|| self.literal()),
            },
            Ty::Int => match self.rng.below(10) {
                0..=5 => {
                    let op = self
                        .rng
                        .pick(&["+", "-", "*", "/", "%", "&+", "&-", "&*", "+", "-"]);
                    let lhs = self.expr(Ty::Int, depth - 1);
                    // Literal right operands exercise the immediate forms.
                    let rhs = if self.rng.chance(40) {
                        self.literal()
                    } else {
                        self.expr(Ty::Int, depth - 1)
                    };
                    format!("({lhs} {op} {rhs})")
                }
                6 => format!("-{}", self.expr(Ty::Int, depth - 1)),
                // Reentry, mostly when there is a `callback` to run.
                7 if self
                    .rng
                    .chance(if self.callback.is_some() { 50 } else { 3 }) =>
                {
                    format!("test.reenter({})", self.expr(Ty::Int, depth - 1))
                }
                7 => format!("test.id({})", self.expr(Ty::Int, depth - 1)),
                _ => self
                    .callee(Ty::Int)
                    .unwrap_or_else(|| self.expr(Ty::Int, depth - 1)),
            },
            Ty::Bool if leaf => match self.rng.below(3) {
                0 => self.rng.pick(&["true", "false"]).to_owned(),
                _ => self
                    .var(Ty::Bool)
                    .unwrap_or_else(|| self.rng.pick(&["true", "false"]).to_owned()),
            },
            Ty::Bool => match self.rng.below(10) {
                0..=3 => {
                    let op = self.rng.pick(&["<", "<=", ">", ">=", "==", "!="]);
                    let lhs = self.expr(Ty::Int, depth - 1);
                    let rhs = if self.rng.chance(40) {
                        self.literal()
                    } else {
                        self.expr(Ty::Int, depth - 1)
                    };
                    format!("({lhs} {op} {rhs})")
                }
                4 | 5 => {
                    let op = self.rng.pick(&["&&", "||", "==", "!="]);
                    let lhs = self.expr(Ty::Bool, depth - 1);
                    let rhs = self.expr(Ty::Bool, depth - 1);
                    format!("({lhs} {op} {rhs})")
                }
                6 => format!("!{}", self.expr(Ty::Bool, depth - 1)),
                7 => format!("test.check({})", self.expr(Ty::Bool, depth - 1)),
                _ => self
                    .callee(Ty::Bool)
                    .unwrap_or_else(|| self.expr(Ty::Bool, depth - 1)),
            },
            Ty::Void => unreachable!("no `void` values"),
        }
    }
}

fn type_name(ty: Ty) -> &'static str {
    match ty {
        Ty::Int => "i64",
        Ty::Bool => "bool",
        Ty::Void => "void",
    }
}
