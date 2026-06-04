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
            let mut init_progress = TerminalInitProgress::start();
            let mut progress = TerminalIndexProgress::start();
            let report =
                app::init_with_progress_and_steps(&mut progress, &mut init_progress).await?;
            progress.finish();
            init_progress.finish(&report);
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

struct TerminalInitProgress {
    step: usize,
}

impl TerminalInitProgress {
    const STEP_COUNT: usize = 5;

    fn start() -> Self {
        println!("cds init");
        flush_stdout();
        Self { step: 0 }
    }

    fn begin(&mut self, label: &str) {
        self.step += 1;
        println!("[{}/{}] {label}", self.step, Self::STEP_COUNT);
        flush_stdout();
    }

    fn detail(label: &str, value: impl std::fmt::Display) {
        println!("      {label}: {value}");
        flush_stdout();
    }

    fn status(label: &str, path: &Path, created: bool) {
        let action = if created { "created" } else { "ready" };
        Self::detail(label, format_args!("{action} {}", path.display()));
    }

    fn finish(&mut self, report: &app::InitReport) {
        Self::detail("index", report.index.human_summary());
        Self::detail("config", report.config_file.display());
        Self::detail("database", report.database_file.display());
        println!("cds init complete");
        flush_stdout();
    }
}

impl app::InitProgress for TerminalInitProgress {
    fn paths_started(&mut self) {
        self.begin("Resolve cds directories");
    }

    fn paths_ready(&mut self, config_dir: &Path, data_dir: &Path, cache_dir: &Path) {
        Self::detail("config dir", config_dir.display());
        Self::detail("data dir", data_dir.display());
        Self::detail("cache dir", cache_dir.display());
    }

    fn config_started(&mut self, path: &Path) {
        self.begin("Prepare config");
        Self::detail("path", path.display());
    }

    fn config_ready(&mut self, path: &Path, created: bool) {
        Self::status("config", path, created);
    }

    fn database_started(&mut self, path: &Path) {
        self.begin("Prepare database");
        Self::detail("path", path.display());
    }

    fn database_ready(&mut self, path: &Path, created: bool) {
        Self::status("database", path, created);
    }

    fn model_started(&mut self, cache_dir: &Path) {
        self.begin("Load embedding model");
        Self::detail("model", "BAAI/bge-small-en-v1.5");
        Self::detail("cache", cache_dir.join("models").display());
    }

    fn model_ready(&mut self, _cache_dir: &Path) {
        Self::detail("model", "ready");
    }

    fn index_started(&mut self, roots: &[String]) {
        self.begin("Index configured roots");
        let roots = if roots.is_empty() {
            "<none>".to_string()
        } else {
            roots.join(", ")
        };
        Self::detail("roots", roots);
    }
}

fn flush_stdout() {
    let _ = io::stdout().flush();
}

const SEARCH_LABEL: &str = "Searching";

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
    eprint!("\r\x1b[2K{}", search_frame(tick));
    let _ = io::stderr().flush();
}

fn clear_search_frame() {
    eprint!("\r\x1b[2K");
    let _ = io::stderr().flush();
}

fn search_frame(tick: usize) -> String {
    let dots = ".".repeat((tick % 3) + 1);
    format!("{SEARCH_LABEL}{dots}")
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
    enabled: bool,
}

impl TerminalIndexProgress {
    fn start() -> Self {
        let state = Arc::new(Mutex::new(ProgressState::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let enabled = io::stderr().is_terminal();
        let worker = enabled.then(|| spawn_progress_worker(Arc::clone(&state), Arc::clone(&stop)));

        Self {
            state,
            stop,
            worker,
            enabled,
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
        if !self.enabled {
            return;
        }

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
    fn search_frame_cycles_dots() {
        assert_eq!(search_frame(0), "Searching.");
        assert_eq!(search_frame(1), "Searching..");
        assert_eq!(search_frame(2), "Searching...");
        assert_eq!(search_frame(3), "Searching.");
    }
}
