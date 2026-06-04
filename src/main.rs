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
use chrono::{Local, TimeZone};
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
        Invocation::Daemon { run_once } => {
            if run_once {
                app::daemon_once().await?;
            } else {
                app::daemon().await?;
            }
        }
        Invocation::DirectoryTypeCount => {
            for count in app::directory_type_counts().await? {
                println!("{}\t{}", count.count, count.label);
            }
        }
        Invocation::DryRun { query } => {
            let report = app::dry_run(query, 10).await?;
            print_dry_run(report);
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
        Invocation::RestartDaemon => {
            let report = app::restart_daemon()?;
            println!("killed {} cds daemon(s)", report.killed_daemons);
            println!("started cds daemon with pid {}", report.pid);
            println!("daemon log: {}", report.log_file.display());
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

fn print_dry_run(report: cds::search::SearchDryRun) {
    println!("query: {}", report.query);
    println!();

    println!("directory cache:");
    println!("  status: {}", report.cache.status.as_str());
    println!("  directories: {}", report.cache.directory_count);
    println!();

    println!("temporal parse:");
    match &report.temporal.matched_phrase {
        Some(phrase) => println!("  matched phrase: {phrase}"),
        None => println!("  matched phrase: (none)"),
    }
    println!("  cleaned query: {}", report.temporal.cleaned_query);
    println!("  semantic query: {}", report.temporal.semantic_query);
    print_temporal_bound("  modified start", report.temporal.start_unix_seconds);
    print_temporal_bound("  modified end", report.temporal.end_unix_seconds);
    println!();

    print_string_section("candidate terms", &report.candidate_terms);
    print_string_section(
        "sql directory candidates",
        &report.sql_candidate_directories,
    );
    print_string_section(
        "fuzzy/partial directory candidates added",
        &report.fuzzy_candidate_directories,
    );

    println!("embedding scores ({}):", report.embedding_scores.len());
    if report.embedding_scores.is_empty() {
        println!("  (none)");
    } else {
        for score in &report.embedding_scores {
            let state = if score.is_current {
                "current"
            } else {
                "history"
            };
            println!(
                "  {:.6}\t{}\tdir={}\tfile={}\t{}",
                score.cosine_score,
                state,
                score.directory_path,
                score.file_path,
                score.content_preview
            );
        }
    }
    println!();

    println!("final scores ({}):", report.results.len());
    if report.results.is_empty() {
        println!("  (none)");
    } else {
        for result in &report.results {
            println!("  {:.3}\t{}", result.score, result.path);
        }
    }
    println!();

    println!("winner:");
    if let Some(winner) = report.results.first() {
        println!("  {:.3}\t{}", winner.score, winner.path);
    } else {
        println!("  (none)");
    }
}

fn print_temporal_bound(label: &str, value: Option<i64>) {
    match value {
        Some(value) => println!("{label}: {value} ({})", format_unix_seconds(value)),
        None => println!("{label}: (none)"),
    }
}

fn format_unix_seconds(value: i64) -> String {
    Local
        .timestamp_opt(value, 0)
        .single()
        .map(|datetime| datetime.format("%Y-%m-%d %H:%M:%S %Z").to_string())
        .unwrap_or_else(|| "invalid local time".to_string())
}

fn print_string_section(label: &str, values: &[String]) {
    println!("{label} ({}):", values.len());
    if values.is_empty() {
        println!("  (none)");
    } else {
        for value in values {
            println!("  {value}");
        }
    }
    println!();
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

const SEARCH_LABEL: &str = "Searching..";
const SEARCH_SPINNER_COLOR: &str = "\x1b[32m";
const SEARCH_SPINNER_RESET: &str = "\x1b[0m";
const SEARCH_SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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
            thread::sleep(Duration::from_millis(160));
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
    let frame = SEARCH_SPINNER_FRAMES[tick % SEARCH_SPINNER_FRAMES.len()];
    format!("{SEARCH_SPINNER_COLOR}{frame}{SEARCH_SPINNER_RESET} {SEARCH_LABEL}")
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
        self.status(&directory.display().to_string());
    }

    fn status(&mut self, message: &str) {
        if !self.enabled {
            return;
        }

        let Ok(mut state) = self.state.lock() else {
            return;
        };

        state.current_directory = Some(message.to_string());
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
    fn search_frame_cycles_dot_spinner() {
        assert_eq!(search_frame(0), "\x1b[32m⠋\x1b[0m Searching..");
        assert_eq!(search_frame(1), "\x1b[32m⠙\x1b[0m Searching..");
        assert_eq!(search_frame(9), "\x1b[32m⠏\x1b[0m Searching..");
        assert_eq!(search_frame(10), "\x1b[32m⠋\x1b[0m Searching..");
    }
}
