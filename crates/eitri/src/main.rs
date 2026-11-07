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
    match context.run(&source, &mut NoHost) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let code = match error {
                eitri::RunError::Compile(_) => ExitCode::FAILURE,
                eitri::RunError::Fault(_) => ExitCode::from(3),
            };
            let name = path.to_string_lossy();
            let mut files = Files::new();
            let id = files.add(&name, &source);
            debug_assert_eq!(id, eitri::Context::SOURCE);
            // Color only for an interactive terminal, honoring NO_COLOR.
            let options = RenderOptions {
                color: stderr().is_terminal() && env::var_os("NO_COLOR").is_none(),
                ..RenderOptions::default()
            };
            let mut renderer = Renderer::new(&files, options);
            let rendered: Vec<String> = error
                .into_diagnostics()
                .iter()
                .map(|d| renderer.render(d))
                .collect();
            // A blank line between diagnostics, as rustc and codespan do.
            eprint!("{}", rendered.join("\n"));
            code
        }
    }
}

/// The CLI registers no host modules, so no script can call one.
struct NoHost;

impl eitri::Host for NoHost {
    fn call(&mut self, _: &mut eitri::Call<'_, '_>, index: u32) -> Result<(), eitri::HostError> {
        unreachable!("no host function {index} is registered")
    }
}
