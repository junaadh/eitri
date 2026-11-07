//! The `eitri` command: file and process I/O around the library's stages.

use std::{
    env, fs,
    io::{IsTerminal, stderr},
    path::Path,
    process::ExitCode,
};

use eitri::diag::{Diagnostic, Files, RenderOptions, Renderer};
use eitri::{Compiled, Context, PrintOptions};

const USAGE: &str = "\
usage: eitri <command> [options] <file>

commands:
  run <file.eit|file.eits>                 compile, or assemble, and run
  check <file.eit|file.eits>               compile, or assemble and verify, without running
  asm [--annotate] [-o <out>] <file.eit>   print the compiled bytecode as .eits
  verify <file.eits>                       assemble and verify .eits
  dump tokens|ast <file.eit>               print the token stream or the syntax tree

options for every command:
  --host <path>=<file>   register the host module `path` from a declaration
                         file of bodiless `pub fn`s; `run` implements none of
                         them, so calling one faults

`eitri <file>` is `eitri run <file>`.
exit status: 0 success, 1 rejected, 2 usage or I/O error, 3 runtime fault
";

/// Exit status for a usage or I/O error.
const USAGE_ERROR: u8 = 2;

fn main() -> ExitCode {
    let all: Vec<String> = env::args().skip(1).collect();
    let mut args = Vec::new();
    let mut hosts = Vec::new();
    let mut rest = all.iter().map(String::as_str);
    while let Some(arg) = rest.next() {
        if arg == "--host" {
            match rest.next().and_then(|spec| spec.split_once('=')) {
                Some(host) => hosts.push(host),
                None => return usage(),
            }
        } else {
            args.push(arg);
        }
    }
    let context = || -> Result<Context, ExitCode> {
        let mut context = Context::new();
        for &(path, file) in &hosts {
            let input = read(file)?;
            if let Err(diagnostics) = context.register_host_module(path, &input.text) {
                report(&input, &diagnostics);
                return Err(ExitCode::FAILURE);
            }
        }
        Ok(context)
    };
    let mut context = match context() {
        Ok(context) => context,
        Err(code) => return code,
    };
    let context = &mut context;
    match args.as_slice() {
        ["run", path] => run(context, path),
        ["check", path] => check(context, path),
        ["verify", path] => verify(context, path),
        ["asm", rest @ ..] => asm(context, rest),
        ["dump", "tokens", path] => dump(context, path, Context::dump_tokens),
        ["dump", "ast", path] => dump(context, path, Context::dump_ast),
        ["help" | "--help" | "-h"] => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        [path] if !is_command(path) => run(context, path),
        _ => usage(),
    }
}

fn is_command(word: &str) -> bool {
    matches!(word, "run" | "check" | "verify" | "asm" | "dump")
}

fn usage() -> ExitCode {
    eprint!("{USAGE}");
    ExitCode::from(USAGE_ERROR)
}

/// A source file and the text it holds.
struct Input<'p> {
    path: &'p str,
    text: String,
}

impl Input<'_> {
    fn is_assembly(&self) -> bool {
        Path::new(self.path)
            .extension()
            .is_some_and(|e| e == "eits")
    }
}

fn read(path: &str) -> Result<Input<'_>, ExitCode> {
    fs::read_to_string(path)
        .map(|text| Input { path, text })
        .map_err(|error| {
            eprintln!("{path}: error[IO]: {error}");
            ExitCode::from(USAGE_ERROR)
        })
}

/// Compile `.eit` or assemble `.eits`, reporting a rejection (exit 1).
fn build(context: &mut Context, input: &Input) -> Result<Compiled, ExitCode> {
    let built = if input.is_assembly() {
        context.assemble(&input.text)
    } else {
        context.compile(&input.text)
    };
    built.map_err(|diagnostics| {
        report(input, &diagnostics);
        ExitCode::FAILURE
    })
}

fn run(context: &mut Context, path: &str) -> ExitCode {
    let input = match read(path) {
        Ok(input) => input,
        Err(code) => return code,
    };
    let compiled = match build(context, &input) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    match context.execute(&compiled, &mut NoHost) {
        Ok(()) => ExitCode::SUCCESS,
        Err(fault) => {
            report(&input, &[fault.diagnostic()]);
            ExitCode::from(3)
        }
    }
}

fn check(context: &mut Context, path: &str) -> ExitCode {
    let input = match read(path) {
        Ok(input) => input,
        Err(code) => return code,
    };
    match build(context, &input) {
        Ok(_) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

fn verify(context: &mut Context, path: &str) -> ExitCode {
    let input = match read(path) {
        Ok(input) => input,
        Err(code) => return code,
    };
    if !input.is_assembly() {
        eprintln!("{path}: `verify` takes a `.eits` file; `check` compiles `.eit`");
        return ExitCode::from(USAGE_ERROR);
    }
    match build(context, &input) {
        Ok(compiled) => {
            println!("{path}: {} functions verified", compiled.function_count());
            ExitCode::SUCCESS
        }
        Err(code) => code,
    }
}

fn asm(context: &mut Context, args: &[&str]) -> ExitCode {
    let mut options = PrintOptions::default();
    let mut output = None;
    let mut path = None;
    let mut rest = args.iter();
    while let Some(&arg) = rest.next() {
        match arg {
            "--annotate" => options.annotate = true,
            "-o" => match rest.next() {
                Some(&out) => output = Some(out),
                None => return usage(),
            },
            _ if path.is_none() && !arg.starts_with('-') => path = Some(arg),
            _ => return usage(),
        }
    }
    let Some(path) = path else {
        return usage();
    };
    let input = match read(path) {
        Ok(input) => input,
        Err(code) => return code,
    };
    let compiled = match build(context, &input) {
        Ok(compiled) => compiled,
        Err(code) => return code,
    };
    let name = Path::new(path)
        .file_name()
        .map_or(path.into(), |name| name.to_string_lossy());
    let text = format!("; {name}\n\n{}", context.disassemble(&compiled, options));
    match output {
        None => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Some(out) => match fs::write(out, text) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{out}: error[IO]: {error}");
                ExitCode::from(USAGE_ERROR)
            }
        },
    }
}

/// Print one stage's dump of a source file.
fn dump(
    context: &mut Context,
    path: &str,
    stage: fn(&mut Context, &str) -> Result<String, Vec<Diagnostic>>,
) -> ExitCode {
    let input = match read(path) {
        Ok(input) => input,
        Err(code) => return code,
    };
    match stage(context, &input.text) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(diagnostics) => {
            report(&input, &diagnostics);
            ExitCode::FAILURE
        }
    }
}

/// Render diagnostics against the input on stderr.
fn report(input: &Input, diagnostics: &[Diagnostic]) {
    let mut files = Files::new();
    let id = files.add(input.path, &input.text);
    debug_assert_eq!(id, Context::SOURCE);
    // Color only for an interactive terminal, honoring NO_COLOR.
    let options = RenderOptions {
        color: stderr().is_terminal() && env::var_os("NO_COLOR").is_none(),
        ..RenderOptions::default()
    };
    let mut renderer = Renderer::new(&files, options);
    let rendered: Vec<String> = diagnostics.iter().map(|d| renderer.render(d)).collect();
    // A blank line between diagnostics, as rustc and codespan do.
    eprint!("{}", rendered.join("\n"));
}

/// The CLI implements no host functions: `--host` only declares them.
struct NoHost;

impl eitri::Host for NoHost {
    fn call(&mut self, _: &mut eitri::Call<'_, '_>, _: u32) -> Result<(), eitri::HostError> {
        Err("`eitri run` implements no host functions".into())
    }
}
