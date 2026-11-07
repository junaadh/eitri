use std::{
    env, fs,
    io::{IsTerminal, stderr},
    process::ExitCode,
};

use eitri::diag::{Files, RenderOptions, Renderer};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: eitri <source.eit>");
        return ExitCode::from(2);
    };
    if args.next().is_some() {
        eprintln!("usage: eitri <source.eit>");
        return ExitCode::from(2);
    }

    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("{}: error[IO]: {error}", path.to_string_lossy());
            return ExitCode::from(2);
        }
    };
    let mut context = eitri::Context::new();
    match context.run(&source) {
        Ok(()) => ExitCode::SUCCESS,
        Err(diagnostic) => {
            let name = path.to_string_lossy();
            let mut files = Files::new();
            let id = files.add(&name, &source);
            debug_assert_eq!(id, eitri::Context::SOURCE);
            // Color only for an interactive terminal, honoring NO_COLOR.
            let options = RenderOptions {
                color: stderr().is_terminal() && env::var_os("NO_COLOR").is_none(),
                ..RenderOptions::default()
            };
            eprint!("{}", Renderer::new(&files, options).render(&diagnostic));
            ExitCode::FAILURE
        }
    }
}
