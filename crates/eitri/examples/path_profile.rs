//! Isolated sampling targets and per-stage timing on the T011 fixtures.
//! See benches/README.md for commands and measurement boundaries.
use std::hint::black_box;
use std::time::{Duration, Instant};

use eitri::{Context, Host};
use eitri_base::{Interner, SourceId};
use eitri_check::{CheckScratch, Hosts, Input};
use eitri_diag::Diagnostics;
use eitri_lower::LowerScratch;
use eitri_syntax::{LexScratch, ModuleKind, ParseScratch};
use eitri_vm::{Limits, VmScratch};

const DECLARATIONS: &str = "pub fn emit(value: i64) -> void\npub fn id(value: i64) -> i64\n";

struct Output(Option<i64>);
impl Host for Output {
    fn call(&mut self, index: u32, args: &[u64], results: &mut [u64]) -> Result<(), String> {
        match index {
            0 => {
                assert!(self.0.replace(args[0] as i64).is_none());
            }
            1 => results[0] = args[0],
            _ => panic!("unknown host index"),
        }
        Ok(())
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("vm");
    let name = args.get(2).map(String::as_str).unwrap_or("calls");
    let default = if mode == "stages" { 20_001 } else { 5 };
    let amount: u64 = args.get(3).map_or(default, |s| s.parse().unwrap());
    assert!(amount > 0);
    let answer = match name {
        "module" => 754,
        "arithmetic" => 7_914_953_978_373_240_671,
        "branching" => 1622,
        "calls" => 610,
        "host_calls" => 499_500,
        _ => panic!("unknown T011 workload"),
    };
    let source = std::fs::read_to_string(format!(
        "{}/benches/fixtures/{name}.eit",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let mut host = Output(None);
    if mode == "vm" || mode == "compile" {
        let mut context = Context::new();
        // EITRI_THREADED=0 selects the `match` interpreter; threaded is the default.
        context.set_threaded(std::env::var_os("EITRI_THREADED").is_none_or(|v| v != "0"));
        context.register_host_module("bench", DECLARATIONS).unwrap();
        assert_eq!(context.host_index("bench", "emit"), Some(0));
        let compiled = context.compile(&source).unwrap();
        context.execute(&compiled, &mut host).unwrap();
        assert_eq!(host.0.take(), Some(answer));
        let start = Instant::now();
        let mut runs = 0u64;
        while start.elapsed() < Duration::from_secs(amount) {
            for _ in 0..256 {
                if mode == "vm" {
                    context.execute(black_box(&compiled), &mut host).unwrap();
                    assert_eq!(host.0.take(), Some(answer));
                } else {
                    drop(black_box(context.compile(black_box(&source)).unwrap()));
                }
            }
            runs += 256;
        }
        println!(
            "{mode} {name}: {runs} runs, {:.1} ns/run (aggregate throughput, not latency tails)",
            start.elapsed().as_nanos() as f64 / runs as f64
        );
        return;
    }
    assert!(
        mode == "stages" || mode == "lower",
        "modes: vm, compile, lower, stages"
    );
    let mut interner = Interner::new();
    let mut hosts = Hosts::new();
    hosts
        .register("bench", DECLARATIONS, &mut interner)
        .unwrap();
    let (mut lex, mut parse, mut check, mut lower, mut vm) = (
        LexScratch::new(),
        ParseScratch::new(),
        CheckScratch::new(),
        LowerScratch::new(),
        VmScratch::new(),
    );
    let mut csv = String::from("sample,lex_ns,parse_ns,check_ns,lower_ns,drop_ns\n");
    let start = Instant::now();
    let mut runs = 0;
    loop {
        interner.reset();
        let mut diagnostics = Diagnostics::new(100);
        let t0 = Instant::now();
        let tokens = eitri_syntax::lex(
            black_box(&source),
            SourceId::from_u32(0),
            &mut interner,
            &mut diagnostics,
            &mut lex,
        );
        let t1 = Instant::now();
        let ast = eitri_syntax::parse(
            &source,
            SourceId::from_u32(0),
            tokens,
            ModuleKind::Script,
            &mut diagnostics,
            &mut parse,
        );
        let t2 = Instant::now();
        let input = Input {
            text: &source,
            source: SourceId::from_u32(0),
            tokens,
            ast,
            interner: &interner,
            hosts: &hosts,
        };
        let checked = eitri_check::check(&input, &mut diagnostics, &mut check);
        let t3 = Instant::now();
        assert!(diagnostics.is_empty());
        let program = eitri_lower::lower(&input, checked, &mut lower).unwrap();
        let t4 = Instant::now();
        // Validate each fresh artifact outside the timed compiler phases.
        eitri_vm::run(
            &program.functions,
            program.entry,
            &[],
            &mut host,
            Limits::default(),
            &mut vm,
        )
        .unwrap();
        assert_eq!(host.0.take(), Some(answer));
        if mode == "lower" {
            while start.elapsed() < Duration::from_secs(amount) {
                for _ in 0..256 {
                    drop(black_box(
                        eitri_lower::lower(black_box(&input), checked, &mut lower).unwrap(),
                    ));
                }
                runs += 256;
            }
            println!("lower {name}: {runs} runs (lower + artifact disposal; frontend built once)");
            return;
        }
        let t5 = Instant::now();
        drop(black_box(program));
        let t6 = Instant::now();
        if runs >= 1000 {
            use std::fmt::Write;
            writeln!(
                csv,
                "{},{},{},{},{},{}",
                runs - 1000,
                (t1 - t0).as_nanos(),
                (t2 - t1).as_nanos(),
                (t3 - t2).as_nanos(),
                (t4 - t3).as_nanos(),
                (t6 - t5).as_nanos()
            )
            .unwrap();
        }
        runs += 1;
        if runs == amount + 1000 {
            break;
        }
    }
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench-results");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("stages-{name}.csv"));
    std::fs::write(&path, csv).unwrap();
    println!("raw individual stage samples: {}", path.display());
}
