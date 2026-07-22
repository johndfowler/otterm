//! Otterm — the output librarian. `otterm run -- <cmd>` captures a command's
//! output; bare `otterm` opens the TUI library to browse and search it.

mod banner;
mod capture;
mod store;
mod tui;

use std::io::{self, Read, Write};

use clap::{Parser, Subcommand};

use store::Store;

#[derive(Parser)]
#[command(name = "otterm", version, about = "The output librarian ~( o.o )~")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a command, streaming its output while capturing it to the library
    Run {
        /// The command and its arguments (use `--` before flags: otterm run -- ls -la)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        command: Vec<String>,
    },
    /// Print the most recent run's captured output to stdout (pipeable)
    Last,
}

fn main() {
    let cli = Cli::parse();
    let result = Store::open().and_then(|store| match cli.cmd {
        Some(Cmd::Run { command }) => {
            // Propagate the child's exit code so `otterm run` is transparent
            // to scripts, CI, and && chains.
            let code = capture::run_command(&store, &command)?;
            std::process::exit(code);
        }
        Some(Cmd::Last) => print_last(&store),
        None => tui::run(store),
    });
    if let Err(e) = result {
        eprintln!("otterm: {e}");
        std::process::exit(1);
    }
}

fn print_last(store: &Store) -> io::Result<()> {
    let runs = store.list()?;
    let Some(meta) = runs.last() else {
        eprintln!("otterm: no runs captured yet");
        std::process::exit(1);
    };
    // Stream the raw bytes — colors intact if stdout is a terminal.
    let mut f = std::fs::File::open(store.output_path(&meta.id))?;
    let mut out = io::stdout().lock();
    let mut buf = [0u8; 65536];
    loop {
        match f.read(&mut buf)? {
            0 => break,
            n => out.write_all(&buf[..n])?,
        }
    }
    out.flush()
}
