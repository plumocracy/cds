use std::env;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cds::app;
use cds::cli::{Invocation, parse_invocation};
use cds::index::IndexProgress;
use cds::{error, shell_init};
use color_eyre::eyre::{Result, WrapErr};

fn main() -> ExitCode {
    if let Err(err) = install_error_reporter() {
        eprintln!("cds: failed to install error reporter: {err}");
        return ExitCode::FAILURE;
    }

    let invocation = match parse_invocation(env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(err) => err.exit(),
    };

    match run(invocation) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let report = color_eyre::eyre::Report::from(err);
            eprintln!("{report:?}");
            ExitCode::FAILURE
        }
    }
}

fn install_error_reporter() -> Result<()> {
    color_eyre::config::HookBuilder::default()
        .display_location_section(false)
        .display_env_section(false)
        .install()
        .wrap_err("failed to install color-eyre error reporter")
}

fn run(invocation: Invocation) -> error::Result<()> {
    match invocation {
        Invocation::DirectoryTypeCount => {
            for count in app::directory_type_counts()? {
                println!("{}\t{}", count.count, count.label);
            }
        }
        Invocation::EmitCd { args } => write_stdout(&app::resolve_cd_script(args))?,
        Invocation::ShellInit { shell } => write_stdout(shell_init(shell).as_bytes())?,
        Invocation::Init => {
            let mut progress = TerminalIndexProgress::start();
            let report = app::init_with_progress(&mut progress)?;
            progress.finish();
            println!("config ready: {}", report.config_file.display());
            println!("database ready: {}", report.database_file.display());
            println!("{}", report.index.human_summary());
        }
        Invocation::Index { roots } => {
            let mut progress = TerminalIndexProgress::start();
            let report = app::index_with_progress(roots, &mut progress)?;
            progress.finish();
            println!("{}", report.human_summary());
        }
        Invocation::Reset => {
            if confirm_reset()? {
                app::reset_database()?;
                println!("database reset");
            } else {
                println!("reset cancelled");
            }
        }
        Invocation::Search { query } => {
            let results = app::search(query, 10)?;
            for result in results {
                println!("{:.3}\t{}", result.score, result.path);
            }
        }
    }

    Ok(())
}

fn write_stdout(bytes: &[u8]) -> error::Result<()> {
    io::stdout().write_all(bytes).map_err(error::Error::Stdout)
}

fn confirm_reset() -> error::Result<bool> {
    print!(
        "This will delete all data in the cds database and it is irreversable. Continue [y/n]? "
    );
    io::stdout().flush().map_err(error::Error::Stdout)?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(error::Error::Stdin)?;

    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

#[derive(Debug, Default)]
struct ProgressState {
    current_directory: Option<String>,
    tick: usize,
    last_len: usize,
}

struct TerminalIndexProgress {
    state: Arc<Mutex<ProgressState>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl TerminalIndexProgress {
    fn start() -> Self {
        let state = Arc::new(Mutex::new(ProgressState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Some(spawn_progress_worker(Arc::clone(&state), Arc::clone(&stop)));

        Self {
            state,
            stop,
            worker,
        }
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for TerminalIndexProgress {
    fn drop(&mut self) {
        self.finish();
    }
}

impl IndexProgress for TerminalIndexProgress {
    fn directory_started(&mut self, directory: &Path) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        state.current_directory = Some(directory.display().to_string());
        state.tick = 0;
        render_progress_line(&mut state);
    }
}

fn spawn_progress_worker(
    state: Arc<Mutex<ProgressState>>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            if let Ok(mut state) = state.lock() {
                render_progress_line(&mut state);
            }

            thread::sleep(Duration::from_millis(180));
        }

        if let Ok(mut state) = state.lock()
            && state.last_len > 0
        {
            eprint!("\r{}\r", " ".repeat(state.last_len));
            let _ = io::stderr().flush();
            state.last_len = 0;
        }
    })
}

fn render_progress_line(state: &mut ProgressState) {
    let Some(directory) = &state.current_directory else {
        return;
    };

    let dots = ".".repeat((state.tick % 3) + 1);
    let line = format!("Indexing: {directory}{dots}");
    let padding = " ".repeat(state.last_len.saturating_sub(line.len()));

    eprint!("\r{line}{padding}");
    let _ = io::stderr().flush();

    state.last_len = line.len();
    state.tick = state.tick.wrapping_add(1);
}
