//! Printing functions as `.eits`.

use std::collections::BTreeSet;
use std::fmt::Write;

use super::{HostDecl, MODULE, mnemonic, type_name, uncomplement};
use eitri_base::Idx;

use crate::encoding::{Decoded, decode};
use crate::function::{CallTarget, Function, ValueType};
use crate::spec::{Op, Role, Sem};

#[derive(Clone, Copy, Debug, Default)]
pub struct PrintOptions {
    /// Add each instruction's PC, words, call-site index, and source span
    /// as a comment.
    pub annotate: bool,
}

/// Print a program: `declare` for each host function its calls use, then
/// `define` for each function, in order. `hosts` is indexed by host index.
pub fn print(functions: &[Function], hosts: &[HostDecl], options: PrintOptions) -> String {
    let mut out = String::new();
    let used: BTreeSet<u32> = functions
        .iter()
        .flat_map(|f| f.sites.iter())
        .filter_map(|site| match site.target {
            CallTarget::Host(index) => Some(index),
            CallTarget::Script(_) => None,
        })
        .collect();
    for &index in &used {
        match hosts.get(index as usize) {
            Some(host) => {
                let params: Vec<&str> = host
                    .signature
                    .params
                    .iter()
                    .map(|&ty| type_name(Some(ty)))
                    .collect();
                let ret = type_name(host.signature.results.first().copied());
                let name = global(&host.name);
                writeln!(out, "declare {ret} @{name}({})", params.join(", ")).unwrap();
            }
            None => writeln!(out, "; host function {index} is not declared").unwrap(),
        }
    }
    for function in functions {
        if !out.is_empty() {
            out.push('\n');
        }
        Printer {
            out: &mut out,
            function,
            functions,
            hosts,
            options,
        }
        .function();
    }
    out
}

/// A name as `@` writes it: bare when it is an identifier (dots allowed),
/// `<module>`, or quoted.
fn global(name: &str) -> String {
    let plain = name == MODULE
        || name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
    if plain {
        name.to_owned()
    } else {
        format!("\"{name}\"")
    }
}

/// One logical instruction: a decoded instruction, or a far branch (its
/// complement and the `J_X` after it) folded back into one.
struct Item {
    pc: usize,
    /// Words, both instructions of a far branch included.
    words: usize,
    decoded: Decoded,
    /// For a branch: its target PC.
    target: Option<usize>,
}

struct Printer<'a> {
    out: &'a mut String,
    function: &'a Function,
    functions: &'a [Function],
    hosts: &'a [HostDecl],
    options: PrintOptions,
}

impl Printer<'_> {
    fn function(&mut self) {
        let f = self.function;
        let ret = type_name(f.signature.results.first().copied());
        let base = f.signature.param_base();
        let params: Vec<String> = f
            .signature
            .params
            .iter()
            .enumerate()
            .map(|(i, &ty)| format!("{} %r{}", type_name(Some(ty)), base + i as u32))
            .collect();
        writeln!(
            self.out,
            "define {ret} @{}({}) frame({}) {{",
            global(&f.name),
            params.join(", "),
            f.frame_size
        )
        .unwrap();
        let Some(items) = self.items() else {
            // Not decodable: show the words rather than guess.
            for (pc, word) in f.code.iter().enumerate() {
                writeln!(self.out, "    ; {pc:04}  {word:#010x}").unwrap();
            }
            self.out.push_str("}\n");
            return;
        };
        // Blocks start at the entry, at branch targets, and after branches
        // and returns; they are numbered in PC order.
        let mut leaders = BTreeSet::from([0usize]);
        for (i, item) in items.iter().enumerate() {
            if let Some(target) = item.target {
                leaders.insert(target);
            }
            let info = item.decoded.op.info();
            let ends = item.target.is_some() || matches!(info.sem, Sem::Return);
            if ends && let Some(next) = items.get(i + 1) {
                leaders.insert(next.pc);
            }
        }
        let label = |pc: usize| {
            leaders
                .iter()
                .position(|&l| l == pc)
                .map(|n| format!("bb{n}"))
        };
        for item in &items {
            if let Some(name) = label(item.pc) {
                writeln!(self.out, "{name}:").unwrap();
            }
            let text = self.instruction(item, &label);
            if self.options.annotate {
                let note = self.annotation(item);
                writeln!(self.out, "    {text:<44}; {note}").unwrap();
            } else {
                writeln!(self.out, "    {text}").unwrap();
            }
        }
        self.out.push_str("}\n");
    }

    /// Decode the code into logical instructions, or `None` if it does not
    /// decode.
    fn items(&self) -> Option<Vec<Item>> {
        let code = &self.function.code;
        let mut decoded = Vec::new();
        let mut pc = 0;
        while pc < code.len() {
            let insn = decode(code, pc).ok()?;
            decoded.push((pc, insn));
            pc += insn.words as usize;
        }
        let mut items = Vec::with_capacity(decoded.len());
        let mut i = 0;
        while i < decoded.len() {
            let (pc, insn) = decoded[i];
            let end = pc + insn.words as usize;
            let target = branch_target(&insn, end);
            // A far branch: the complement skipping exactly the `J_X` after it.
            if let (Some(skip), Some(&(jpc, jump))) = (target, decoded.get(i + 1))
                && jump.op == Op::JX
                && skip == jpc + jump.words as usize
                && let Some((op, a, b)) = uncomplement(insn.op, insn.operands.a, insn.operands.b)
            {
                let mut logical = insn;
                logical.op = op;
                logical.operands.a = a;
                logical.operands.b = b;
                items.push(Item {
                    pc,
                    words: insn.words as usize + jump.words as usize,
                    decoded: logical,
                    target: branch_target(&jump, skip),
                });
                i += 2;
                continue;
            }
            items.push(Item {
                pc,
                words: insn.words as usize,
                decoded: insn,
                target,
            });
            i += 1;
        }
        Some(items)
    }

    fn instruction(&self, item: &Item, label: &dyn Fn(usize) -> Option<String>) -> String {
        let f = self.function;
        let d = &item.decoded;
        let op = d.op;
        let info = op.info();
        let to = || {
            let target = item.target.expect("branches have targets");
            match label(target) {
                Some(name) => format!("label %{name}"),
                None => format!("label %pc{target}"),
            }
        };
        let name = mnemonic(op);
        match info.sem {
            Sem::LoadImm | Sem::LoadConst => {
                let (value, index) = match op {
                    Op::Ldi => (Some(d.operands.b as u64), 0),
                    Op::Ldk => (f.consts.get(d.operands.b as usize).copied(), d.operands.b),
                    _ => (
                        f.consts.get(d.operands.ext as usize).copied(),
                        d.operands.ext,
                    ),
                };
                let Some(value) = value else {
                    return format!("%r{} = {name} ; no constant k{index}", d.operands.a);
                };
                let ty = f.literal_type(item.pc as u32);
                let value = match ty {
                    Some(ValueType::Bool) if value <= 1 => (value == 1).to_string(),
                    _ => (value as i64).to_string(),
                };
                let ty = ty.map_or("?", |ty| type_name(Some(ty)));
                format!("%r{} = {name} {ty} {value}", d.operands.a)
            }
            Sem::Call(_) => self.call(&name, d.operands.b as usize),
            Sem::Return => name,
            Sem::Jump => format!("j {}", to()),
            Sem::Branch { imm: false, .. } => {
                format!("{name} %r{}, %r{}, {}", d.operands.a, d.operands.b, to())
            }
            Sem::Branch { imm: true, .. } => {
                format!("{name} %r{}, {}, {}", d.operands.a, d.operands.b, to())
            }
            Sem::TestBranch(_) => format!("{name} %r{}, {}", d.operands.a, to()),
            _ => {
                let columns = [d.operands.a, d.operands.b, d.operands.c];
                let mut operands = Vec::new();
                let mut dest = None;
                for (i, (&role, &value)) in info.roles.iter().zip(&columns).enumerate() {
                    match role {
                        Role::None => {}
                        Role::Dst if i == 0 => dest = Some(value),
                        role => operands.push(operand(role, value, item, label)),
                    }
                }
                let ext = info.format.ext_role();
                if ext != Role::None {
                    operands.push(operand(ext, d.operands.ext, item, label));
                }
                let rest = if operands.is_empty() {
                    name
                } else {
                    format!("{name} {}", operands.join(", "))
                };
                match dest {
                    Some(slot) => format!("%r{slot} = {rest}"),
                    None => rest,
                }
            }
        }
    }

    fn call(&self, name: &str, index: usize) -> String {
        let Some(site) = self.function.sites.get(index) else {
            return format!("{name} ; no call site {index}");
        };
        let (callee, signature) = match site.target {
            CallTarget::Script(id) => match self.functions.get(id.index()) {
                Some(f) => (global(&f.name), &f.signature),
                None => return format!("{name} ; no function {}", id.index()),
            },
            CallTarget::Host(h) => match self.hosts.get(h as usize) {
                Some(host) => (global(&host.name), &host.signature),
                None => return format!("{name} ; no host function {h}"),
            },
        };
        let args: Vec<String> = signature
            .params
            .iter()
            .enumerate()
            .map(|(i, &ty)| {
                let slot = site.args.first + i as u32;
                format!("{} %r{slot}", type_name(Some(ty)))
            })
            .collect();
        let ret = signature.results.first().copied();
        let call = format!("{name} {} @{callee}({})", type_name(ret), args.join(", "));
        match ret {
            Some(_) => format!("%r{} = {call}", site.result.first),
            None => call,
        }
    }

    fn annotation(&self, item: &Item) -> String {
        let f = self.function;
        let words: Vec<String> = f.code[item.pc..item.pc + item.words]
            .iter()
            .map(|word| format!("{word:08x}"))
            .collect();
        let mut note = format!("{:04}  {}", item.pc, words.join(" "));
        if matches!(item.decoded.op.info().sem, Sem::Call(_)) {
            write!(note, "  site{}", item.decoded.operands.b).unwrap();
        }
        if let Some(span) = f.span_at(item.pc as u32) {
            write!(note, "  @{}..{}", span.start, span.end).unwrap();
        }
        note
    }
}

fn branch_target(insn: &Decoded, end: usize) -> Option<usize> {
    let info = insn.op.info();
    let disp = info
        .roles
        .iter()
        .zip([insn.operands.a, insn.operands.b, insn.operands.c])
        .find(|(role, _)| role.is_displacement())
        .map(|(_, value)| value)
        .or_else(|| {
            info.format
                .ext_role()
                .is_displacement()
                .then_some(insn.operands.ext)
        })?;
    usize::try_from(end as i64 + disp).ok()
}

/// A plain instruction's operand in the role's text form.
fn operand(role: Role, value: i64, item: &Item, label: &dyn Fn(usize) -> Option<String>) -> String {
    match role {
        Role::Dst | Role::Src | Role::SrcDst => format!("%r{value}"),
        role if role.is_displacement() => match item.target.and_then(label) {
            Some(name) => format!("label %{name}"),
            None => format!("{value:+}"),
        },
        _ => value.to_string(),
    }
}
