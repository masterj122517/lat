mod app;
mod discovery;
mod tmux;
mod tracker;

use std::io::{self, IsTerminal};

fn main() {
    if let Err(error) = run() {
        eprintln!("lat: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [arg] if arg == "--help" => {
            println!(
                "lat — local agent tracker\n\nUsage: lat [--help | --version]\n\nDiscover omp, Codex, and OpenCode; inspect output and jump to their tmux panes."
            );
            return Ok(());
        }
        [arg] if arg == "--version" => {
            println!("lat {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        [] => {}
        _ => return Err("Usage: lat [--help | --version]".into()),
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "interactive mode requires terminal stdin and stdout; run lat in a terminal".into(),
        );
    }
    app::run()?;
    Ok(())
}
