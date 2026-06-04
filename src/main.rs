use std::env;
use std::io::{self, IsTerminal, Write};
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

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(err) = install_error_reporter() {
        eprintln!("cds: failed to install error reporter: {err}");
        return ExitCode::FAILURE;
    }

    let invocation = match parse_invocation(env::args_os().skip(1)) {
        Ok(invocation) => invocation,
        Err(err) => err.exit(),
    };

    match run(invocation).await {
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

async fn run(invocation: Invocation) -> error::Result<()> {
    match invocation {
        Invocation::DirectoryTypeCount => {
            for count in app::directory_type_counts().await? {
                println!("{}\t{}", count.count, count.label);
            }
        }
        Invocation::EmitCd { args } => {
            let mut animation = app::implied_search_query(&args)
                .is_some()
                .then(SearchAnimation::start);
            let script = app::resolve_cd_script(args).await;
            if let Some(animation) = &mut animation {
                animation.finish();
            }

            write_stdout(&script)?;
        }
        Invocation::ShellInit { shell } => write_stdout(shell_init(shell).as_bytes())?,
        Invocation::Init => {
            let mut progress = TerminalIndexProgress::start();
            let report = app::init_with_progress(&mut progress).await?;
            progress.finish();
            println!("config ready: {}", report.config_file.display());
            println!("database ready: {}", report.database_file.display());
            println!("{}", report.index.human_summary());
        }
        Invocation::Index { roots } => {
            let mut progress = TerminalIndexProgress::start();
            let report = app::index_with_progress(roots, &mut progress).await?;
            progress.finish();
            println!("{}", report.human_summary());
        }
        Invocation::Reset => {
            if confirm_reset()? {
                app::reset_database().await?;
                println!("database reset");
            } else {
                println!("reset cancelled");
            }
        }
        Invocation::Search { query } => {
            let mut animation = SearchAnimation::start();
            let results = app::search(query, 10).await;
            animation.finish();

            let results = results?;
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

const SEARCH_LABEL: &str = "searching...";

struct SearchAnimation {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl SearchAnimation {
    fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));

        if !io::stderr().is_terminal() {
            return Self { stop, worker: None };
        }

        let worker = Some(spawn_search_animation(Arc::clone(&stop)));
        Self { stop, worker }
    }

    fn finish(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for SearchAnimation {
    fn drop(&mut self) {
        self.finish();
    }
}

fn spawn_search_animation(stop: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut tick = 0;

        while !stop.load(Ordering::Relaxed) {
            render_search_frame(tick);
            tick = tick.wrapping_add(1);
            thread::sleep(Duration::from_millis(120));
        }

        clear_search_frame();
    })
}

fn render_search_frame(tick: usize) {
    let (top, bottom) = search_frame(tick);
    eprint!("\r\x1b[2K{top}\n\r\x1b[2K{bottom}\x1b[1A");
    let _ = io::stderr().flush();
}

fn clear_search_frame() {
    eprint!("\r\x1b[2K\n\r\x1b[2K\x1b[1A\r");
    let _ = io::stderr().flush();
}

fn search_frame(tick: usize) -> (String, String) {
    let chars = SEARCH_LABEL.chars().collect::<Vec<_>>();
    let active = tick % chars.len();
    let mut top = String::with_capacity(chars.len());
    let mut bottom = String::with_capacity(chars.len());

    for (index, ch) in chars.into_iter().enumerate() {
        if index == active {
            top.push(ch);
            bottom.push(' ');
        } else {
            top.push(' ');
            bottom.push(ch);
        }
    }

    (top, bottom)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_frame_bounces_one_letter_at_a_time() {
        let (top, bottom) = search_frame(0);
        assert_eq!(top.chars().next(), Some('s'));
        assert_eq!(bottom.chars().next(), Some(' '));
        assert_eq!(bottom.chars().collect::<String>(), " earching...");

        let (top, bottom) = search_frame(1);
        assert_eq!(top.chars().nth(1), Some('e'));
        assert_eq!(bottom.chars().nth(1), Some(' '));
        assert_eq!(bottom.chars().collect::<String>(), "s arching...");
    }
}
