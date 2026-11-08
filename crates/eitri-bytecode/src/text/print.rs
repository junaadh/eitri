//! Printing functions as `.eits`, from the logical instructions recorded
//! when they were assembled ([`crate::ir`]).

use std::collections::BTreeSet;
use std::fmt::Write;

use eitri_base::Idx;

use super::{HostDecl, MODULE, mnemonic, type_name};
use crate::asm::{compare_imm_op, compare_op, test_op};
use crate::encoding::decode;
use crate::function::{CallSite, CallTarget, Function, ValueType};
use crate::ir::{Body, Branch, Insn, Item};
use crate::spec::Role;

#[derive(Clone, Copy, Debug, Default)]
pub struct PrintOptions {
    /// Add each instruction's PC, words, call-site index, and source span
    /// as a comment.
    pub annotate: bool,
}

/// Print a program: `declare` for each host function its calls use, then
/// `define` for each function, in order. `bodies` are the functions'
/// recorded logical instructions, one per function; `hosts` is indexed by
/// host index.
pub fn print(
    functions: &[Function],
    bodies: &[Body],
    hosts: &[HostDecl],
    options: PrintOptions,
) -> String {
    assert_eq!(functions.len(), bodies.len(), "one body per function");
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
    for (function, body) in functions.iter().zip(bodies) {
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
        .function(body);
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

struct Printer<'a> {
    out: &'a mut String,
    function: &'a Function,
    functions: &'a [Function],
    hosts: &'a [HostDecl],
    options: PrintOptions,
}

impl Printer<'_> {
    fn function(&mut self, body: &Body) {
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
        // Blocks start at the entry, at labels, and after branches and
        // returns; they are numbered in order. Labels at one point share
        // their block's name.
        let mut block_of_label = vec![0u32; body.labels as usize];
        let mut starts = vec![None; body.items.len()];
        let mut blocks = 0u32;
        let mut pending: Option<u32> = None;
        let mut leader = true;
        for (i, item) in body.items.iter().enumerate() {
            let mut open = || {
                *pending.get_or_insert_with(|| {
                    blocks += 1;
                    blocks - 1
                })
            };
            match item.insn {
                Insn::Label(n) => block_of_label[n as usize] = open(),
                insn => {
                    if leader {
                        open();
                    }
                    starts[i] = pending.take();
                    leader = matches!(insn, Insn::Branch { .. } | Insn::Ret);
                }
            }
        }
        for (i, item) in body.items.iter().enumerate() {
            if matches!(item.insn, Insn::Label(_)) {
                continue;
            }
            if let Some(block) = starts[i] {
                writeln!(self.out, "bb{block}:").unwrap();
            }
            let text = self.instruction(item, &block_of_label);
            if self.options.annotate {
                let note = self.annotation(item);
                writeln!(self.out, "    {text:<44}; {note}").unwrap();
            } else {
                writeln!(self.out, "    {text}").unwrap();
            }
        }
        self.out.push_str("}\n");
    }

    fn instruction(&self, item: &Item, block_of_label: &[u32]) -> String {
        let to = |label: u32| format!("label %bb{}", block_of_label[label as usize]);
        match item.insn {
            Insn::Label(_) => unreachable!("labels print as block names"),
            Insn::Load { dst, value, ty } => {
                // The form the assembler chose, from the words.
                let op = decode(&self.function.code, item.pc as usize).map(|d| d.op);
                let name = op.map_or_else(|_| "ldi".to_owned(), mnemonic);
                let value = match ty {
                    ValueType::Bool if value <= 1 => (value == 1).to_string(),
                    _ => (value as i64).to_string(),
                };
                format!("%r{dst} = {name} {} {value}", type_name(Some(ty)))
            }
            Insn::Plain { op, operands } => {
                let info = op.info();
                let name = mnemonic(op);
                let columns = [operands.a, operands.b, operands.c, operands.ext];
                let roles = [
                    info.roles[0],
                    info.roles[1],
                    info.roles[2],
                    info.format.ext_role(),
                ];
                let writes = roles[0] == Role::Dst;
                let rest: Vec<String> = roles
                    .iter()
                    .zip(columns)
                    .skip(usize::from(writes))
                    .filter(|(role, _)| **role != Role::None)
                    .map(|(&role, value)| match role {
                        Role::Dst | Role::Src | Role::SrcDst => format!("%r{value}"),
                        _ => value.to_string(),
                    })
                    .collect();
                let text = if rest.is_empty() {
                    name
                } else {
                    format!("{name} {}", rest.join(", "))
                };
                if writes {
                    format!("%r{} = {text}", operands.a)
                } else {
                    text
                }
            }
            Insn::Branch { branch, target } => match branch {
                Branch::Compare { cond, a, b } => {
                    format!(
                        "{} %r{a}, %r{b}, {}",
                        mnemonic(compare_op(cond)),
                        to(target)
                    )
                }
                Branch::CompareImm { cond, a, imm } => {
                    format!(
                        "{} %r{a}, {imm}, {}",
                        mnemonic(compare_imm_op(cond)),
                        to(target)
                    )
                }
                Branch::Test { test, a } => {
                    format!("{} %r{a}, {}", mnemonic(test_op(test)), to(target))
                }
                Branch::Jump => format!("j {}", to(target)),
            },
            Insn::Call(site) => self.call(site),
            Insn::Ret => "ret".to_owned(),
        }
    }

    fn call(&self, site: CallSite) -> String {
        let (name, callee, signature) = match site.target {
            CallTarget::Script(id) => match self.functions.get(id.index()) {
                Some(f) => ("call.script", global(&f.name), &f.signature),
                None => return format!("call.script ; no function {}", id.index()),
            },
            CallTarget::Host(h) => match self.hosts.get(h as usize) {
                Some(host) => ("call.host", global(&host.name), &host.signature),
                None => return format!("call.host ; no host function {h}"),
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
        let (pc, words) = (item.pc as usize, item.words as usize);
        let words: Vec<String> = f.code[pc..pc + words]
            .iter()
            .map(|word| format!("{word:08x}"))
            .collect();
        let mut note = format!("{pc:04}  {}", words.join(" "));
        if let Insn::Call(_) = item.insn
            && let Ok(decoded) = decode(&f.code, pc)
        {
            write!(note, "  site{}", decoded.operands.b).unwrap();
        }
        if let Some(span) = f.span_at(item.pc) {
            write!(note, "  @{}..{}", span.start, span.end).unwrap();
        }
        note
    }
}
