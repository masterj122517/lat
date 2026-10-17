mod app;
mod attention;
mod command;
mod discovery;
mod notification;
mod terminal;
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
                "lat — local agent tracker\n\nUsage: lat [--watch | --help | --version]\n\nDiscover omp, Codex, and OpenCode; inspect output and jump to their tmux panes or supported native terminals.\nNotify on detected tmux input prompts while the UI is open.\n--watch  Monitor and send desktop notifications without a terminal UI."
            );
            return Ok(());
        }
        [arg] if arg == "--version" => {
            println!("lat {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        [arg] if arg == "--watch" => return watch(),
        [] => {}
        _ => return Err("Usage: lat [--watch | --help | --version]".into()),
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "interactive mode requires terminal stdin and stdout; run lat in a terminal".into(),
        );
    }
    app::run()?;
    Ok(())
}

fn watch() -> Result<(), Box<dyn std::error::Error>> {
    let tracker = tracker::Tracker::new();
    eprintln!("lat: watching agent panes for input prompts; Ctrl-C to stop");
    let mut previous_error = None;
    loop {
        if let (Some(snapshot), _) = tracker.take() {
            let error = snapshot
                .error
                .as_ref()
                .or(snapshot.notification_error.as_ref());
            if error != previous_error.as_ref() {
                if let Some(error) = error {
                    eprintln!("lat: {error}");
                }
                previous_error = error.cloned();
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
