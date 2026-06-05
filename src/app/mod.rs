mod error;

use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{AppPaths, Settings, expand_tilde};
use crate::db::{Database, DirectoryTypeCount};
#[cfg(feature = "real-embedder")]
use crate::embed::BgeSmallEmbedder;
use crate::embed::{Embedder, FakeEmbedder};
use crate::error::{Result, app_err, config_err, embed_err};
use crate::index::{IndexProgress, IndexReport, Indexer, NoopProgress};
use crate::search::{CacheDryRunStatus, SearchCache, SearchDryRun, SearchResult, Searcher};
use serde::{Deserialize, Serialize};

pub use error::AppError;

const DAEMON_SEARCH_TIMEOUT: Duration = Duration::from_millis(900);
const DAEMON_DRY_RUN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    pub config_file: PathBuf,
    pub database_file: PathBuf,
    pub index: IndexReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitConfigReport {
    pub config_file: PathBuf,
    pub created: bool,
}

pub trait InitProgress {
    fn paths_started(&mut self) {}
    fn paths_ready(&mut self, _config_dir: &Path, _data_dir: &Path, _cache_dir: &Path) {}
    fn config_started(&mut self, _path: &Path) {}
    fn config_ready(&mut self, _path: &Path, _created: bool) {}
    fn database_started(&mut self, _path: &Path) {}
    fn database_ready(&mut self, _path: &Path, _created: bool) {}
    fn model_started(&mut self, _cache_dir: &Path) {}
    fn model_ready(&mut self, _cache_dir: &Path) {}
    fn index_started(&mut self, _roots: &[String]) {}
}

#[derive(Debug, Default)]
pub struct NoopInitProgress;

impl InitProgress for NoopInitProgress {}

pub async fn init() -> Result<InitReport> {
    let mut index_progress = NoopProgress;
    let mut init_progress = NoopInitProgress;
    init_with_progress_and_steps(&mut index_progress, &mut init_progress).await
}

pub async fn init_with_progress<P>(progress: &mut P) -> Result<InitReport>
where
    P: IndexProgress + Send + ?Sized,
{
    let mut init_progress = NoopInitProgress;
    init_with_progress_and_steps(progress, &mut init_progress).await
}

pub async fn init_with_progress_and_steps<P, I>(
    progress: &mut P,
    init_progress: &mut I,
) -> Result<InitReport>
where
    P: IndexProgress + Send + ?Sized,
    I: InitProgress + ?Sized,
{
    init_progress.paths_started();
    let paths = AppPaths::discover()?;
    init_progress.paths_ready(&paths.config_dir, &paths.data_dir, &paths.cache_dir);

    init_progress.config_started(&paths.config_file);
    let config_created = !paths.config_file.exists();
    let settings = Settings::load_or_create(&paths.config_file)?;
    init_progress.config_ready(&paths.config_file, config_created);

    init_progress.database_started(&paths.database_file);
    let database_created = !paths.database_file.exists();
    let database = Database::open(&paths.database_file).await?;
    init_progress.database_ready(&paths.database_file, database_created);

    init_progress.model_started(&paths.cache_dir);
    let embedder = RuntimeEmbedder::load(&paths)?;
    init_progress.model_ready(&paths.cache_dir);

    init_progress.index_started(&settings.index.roots);
    let indexer = Indexer::new(&settings, &database, &embedder);
    let index = indexer
        .index_configured_roots_with_progress(progress)
        .await?;

    Ok(InitReport {
        config_file: paths.config_file,
        database_file: paths.database_file,
        index,
    })
}

pub fn init_config() -> Result<InitConfigReport> {
    let paths = AppPaths::discover()?;
    let created = !paths.config_file.exists();
    Settings::load_or_create(&paths.config_file)?;

    Ok(InitConfigReport {
        config_file: paths.config_file,
        created,
    })
}

pub async fn index(roots: Vec<OsString>) -> Result<IndexReport> {
    let mut progress = NoopProgress;
    index_with_progress(roots, &mut progress).await
}

pub async fn index_with_progress<P>(roots: Vec<OsString>, progress: &mut P) -> Result<IndexReport>
where
    P: IndexProgress + Send + ?Sized,
{
    let paths = AppPaths::discover()?;
    let settings = Settings::load_or_create(&paths.config_file)?;
    let database = Database::open(&paths.database_file).await?;
    let embedder = RuntimeEmbedder::load(&paths)?;
    let indexer = Indexer::new(&settings, &database, &embedder);

    if roots.is_empty() {
        return Ok(indexer
            .index_configured_roots_with_progress(progress)
            .await?);
    }

    let roots = roots
        .iter()
        .map(|root| {
            let root = root
                .to_str()
                .ok_or_else(|| app_err(AppError::InvalidIndexRootUtf8 { root: root.clone() }))?;
            expand_tilde(root).map_err(config_err)
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(indexer.index_roots_with_progress(roots, progress).await?)
}

pub async fn search(query: Vec<OsString>, limit: usize) -> Result<Vec<SearchResult>> {
    let query = join_query(query)?;
    search_text(&query, limit).await
}

pub async fn dry_run(query: Vec<OsString>, limit: usize) -> Result<SearchDryRun> {
    let query = join_query(query)?;
    let paths = AppPaths::discover()?;
    daemon_dry_run(&paths, &query, limit)?
        .ok_or_else(|| app_err(AppError::DaemonUnavailable { mode: "dry-run" }))
}

pub async fn search_text(query: &str, limit: usize) -> Result<Vec<SearchResult>> {
    let paths = AppPaths::discover()?;
    if let Some(results) = daemon_search(&paths, query, limit)? {
        return Ok(results);
    }

    eprintln!("cds: warning: daemon unavailable or busy; searching locally");

    let settings = Settings::load_or_create(&paths.config_file)?;
    let database = Database::open_existing(&paths.database_file).await?;
    let embedder = RuntimeEmbedder::load(&paths)?;
    let searcher = Searcher::new_with_settings(&database, &embedder, &settings);
    Ok(searcher.search(query, limit).await?)
}

pub async fn directory_type_counts() -> Result<Vec<DirectoryTypeCount>> {
    let paths = AppPaths::discover()?;
    let database = Database::open_existing(&paths.database_file).await?;
    Ok(database.directory_type_counts().await?)
}

pub async fn reset_database() -> Result<()> {
    let paths = AppPaths::discover()?;
    let database = Database::open_existing(&paths.database_file).await?;
    Ok(database.reset().await?)
}

pub async fn daemon() -> Result<()> {
    daemon_with_options(DaemonOptions::default()).await
}

pub async fn daemon_once() -> Result<()> {
    let paths = AppPaths::discover()?;
    let settings = Settings::load_or_create(&paths.config_file)?;
    let database = Database::open(&paths.database_file).await?;
    let embedder = RuntimeEmbedder::load(&paths)?;
    let indexer = Indexer::new(&settings, &database, &embedder);
    indexer.index_configured_roots().await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonRestartReport {
    pub killed_daemons: usize,
    pub pid: u32,
    pub log_file: PathBuf,
}

pub fn restart_daemon() -> Result<DaemonRestartReport> {
    let paths = AppPaths::discover()?;
    let killed_daemons = stop_cds_daemons(&paths)?;
    let started = start_daemon_process(&paths)?;

    Ok(DaemonRestartReport {
        killed_daemons,
        pid: started.pid,
        log_file: started.log_file,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DaemonOptions {
    poll_interval: Duration,
    run_once: bool,
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(60),
            run_once: false,
        }
    }
}

async fn daemon_with_options(options: DaemonOptions) -> Result<()> {
    let paths = AppPaths::discover()?;
    if !options.run_once {
        stop_cds_daemons(&paths)?;
    }
    let search_listener = if options.run_once {
        empty_daemon_listener()
    } else {
        prepare_daemon_socket(&paths)?
    };
    if !options.run_once {
        write_daemon_pid(&paths, process::id())?;
    }
    let database = Database::open(&paths.database_file).await?;
    let embedder = RuntimeEmbedder::load(&paths)?;
    let mut root_snapshots = HashMap::<PathBuf, RootFileSnapshot>::new();
    let mut search_cache = None;
    let mut last_poll = Instant::now() - options.poll_interval;

    loop {
        handle_daemon_search_requests(
            &search_listener,
            &paths,
            &database,
            &embedder,
            &mut search_cache,
        )
        .await?;

        if last_poll.elapsed() >= options.poll_interval {
            if daemon_index_changed_files(&paths, &database, &embedder, &mut root_snapshots).await?
            {
                search_cache = None;
            }
            last_poll = Instant::now();
        }

        if options.run_once {
            return Ok(());
        }

        thread::sleep(Duration::from_millis(200));
    }
}

async fn daemon_index_changed_files<E>(
    paths: &AppPaths,
    database: &Database,
    embedder: &E,
    root_snapshots: &mut HashMap<PathBuf, RootFileSnapshot>,
) -> Result<bool>
where
    E: Embedder,
{
    let settings = Settings::load_or_create(&paths.config_file)?;
    let roots = settings.expanded_roots().map_err(config_err)?;
    let mut changed_files = Vec::new();
    let mut deleted_files = Vec::new();

    for root in roots {
        let snapshot = daemon_root_file_snapshot(&root, &settings)?;
        if let Some(previous) = root_snapshots.insert(root, snapshot.clone()) {
            let diff = previous.diff(&snapshot);
            changed_files.extend(diff.changed_files);
            deleted_files.extend(diff.deleted_files);
        }
    }

    if !changed_files.is_empty() || !deleted_files.is_empty() {
        let indexer = Indexer::new(&settings, database, embedder);
        indexer
            .index_file_changes(changed_files, deleted_files)
            .await?;
        return Ok(true);
    }

    Ok(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartedDaemon {
    pid: u32,
    log_file: PathBuf,
}

fn daemon_pid_file(paths: &AppPaths) -> PathBuf {
    paths.cache_dir.join("daemon.pid")
}

fn daemon_log_file(paths: &AppPaths) -> PathBuf {
    paths.cache_dir.join("daemon.log")
}

fn write_daemon_pid(paths: &AppPaths, pid: u32) -> Result<()> {
    fs::create_dir_all(&paths.cache_dir).map_err(|source| {
        app_err(AppError::InspectDaemonWatchPath {
            path: paths.cache_dir.clone(),
            source,
        })
    })?;
    fs::write(daemon_pid_file(paths), pid.to_string()).map_err(|source| {
        app_err(AppError::InspectDaemonWatchPath {
            path: daemon_pid_file(paths),
            source,
        })
    })
}

fn start_daemon_process(paths: &AppPaths) -> Result<StartedDaemon> {
    fs::create_dir_all(&paths.cache_dir).map_err(|source| {
        app_err(AppError::InspectDaemonWatchPath {
            path: paths.cache_dir.clone(),
            source,
        })
    })?;
    let log_file = daemon_log_file(paths);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .map_err(|source| {
            app_err(AppError::InspectDaemonWatchPath {
                path: log_file.clone(),
                source,
            })
        })?;
    let stderr = log.try_clone().map_err(|source| {
        app_err(AppError::InspectDaemonWatchPath {
            path: log_file.clone(),
            source,
        })
    })?;
    let executable = env::current_exe()
        .map_err(|source| app_err(AppError::ResolveCurrentExecutable { source }))?;
    let child = Command::new(executable)
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|source| app_err(AppError::StartDaemon { source }))?;
    let pid = child.id();
    write_daemon_pid(paths, pid)?;

    Ok(StartedDaemon { pid, log_file })
}

#[cfg(unix)]
fn stop_cds_daemons(paths: &AppPaths) -> Result<usize> {
    let mut pids = cds_daemon_pids()?;
    pids.sort_unstable();
    pids.dedup();

    for pid in &pids {
        terminate_process(*pid)?;
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let still_running = pids
            .iter()
            .copied()
            .filter(|pid| process_is_alive(*pid))
            .collect::<Vec<_>>();
        if still_running.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            for pid in still_running {
                kill_process(pid)?;
            }
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    let _ = fs::remove_file(daemon_pid_file(paths));
    let _ = fs::remove_file(daemon_socket_path(paths));
    Ok(pids.len())
}

#[cfg(not(unix))]
fn stop_cds_daemons(paths: &AppPaths) -> Result<usize> {
    let _ = fs::remove_file(daemon_pid_file(paths));
    Ok(0)
}

#[cfg(unix)]
fn cds_daemon_pids() -> Result<Vec<u32>> {
    let current_pid = process::id();
    let current_uid = current_uid()?;
    let output = Command::new("ps")
        .args(["-axo", "pid=,uid=,command="])
        .output()
        .map_err(|source| {
            app_err(AppError::DaemonProcessCommand {
                command: "ps",
                source,
            })
        })?;
    if !output.status.success() {
        return Err(app_err(AppError::DaemonProcessStatus {
            command: "ps",
            status: output.status.to_string(),
        }));
    }

    let process_table = String::from_utf8_lossy(&output.stdout);
    Ok(process_table
        .lines()
        .filter_map(|line| cds_pid_from_process_line(line, current_uid, current_pid))
        .collect())
}

#[cfg(unix)]
fn current_uid() -> Result<Option<u32>> {
    let output = Command::new("id").arg("-u").output().map_err(|source| {
        app_err(AppError::DaemonProcessCommand {
            command: "id",
            source,
        })
    })?;
    if !output.status.success() {
        return Err(app_err(AppError::DaemonProcessStatus {
            command: "id",
            status: output.status.to_string(),
        }));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok())
}

#[cfg(unix)]
fn cds_pid_from_process_line(
    line: &str,
    current_uid: Option<u32>,
    current_pid: u32,
) -> Option<u32> {
    let mut parts = line.split_whitespace();
    let pid = parts.next()?.parse::<u32>().ok()?;
    let uid = parts.next()?.parse::<u32>().ok()?;
    let command = parts.next()?;

    if pid == current_pid {
        return None;
    }
    if current_uid.is_some_and(|current_uid| uid != current_uid) {
        return None;
    }
    is_cds_process_command(command).then_some(pid)
}

#[cfg(unix)]
fn is_cds_process_command(command: &str) -> bool {
    let executable_name = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str());
    executable_name == Some("cds")
}

#[cfg(unix)]
fn terminate_process(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .map_err(|source| {
            app_err(AppError::DaemonProcessCommand {
                command: "kill",
                source,
            })
        })?;
    if !status.success() && process_is_alive(pid) {
        return Err(app_err(AppError::DaemonProcessStatus {
            command: "kill",
            status: status.to_string(),
        }));
    }

    Ok(())
}

#[cfg(unix)]
fn kill_process(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status()
        .map_err(|source| {
            app_err(AppError::DaemonProcessCommand {
                command: "kill",
                source,
            })
        })?;
    if !status.success() && process_is_alive(pid) {
        return Err(app_err(AppError::DaemonProcessStatus {
            command: "kill",
            status: status.to_string(),
        }));
    }

    Ok(())
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .is_ok_and(|status| status.success())
}

#[derive(Debug, Serialize, Deserialize)]
struct DaemonSearchRequest {
    query: String,
    limit: usize,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct DaemonSearchResponse {
    results: Vec<SearchResult>,
    dry_run: Option<SearchDryRun>,
}

#[cfg(unix)]
type DaemonSearchListener = Option<std::os::unix::net::UnixListener>;

#[cfg(not(unix))]
struct DaemonSearchListener;

#[cfg(unix)]
fn daemon_socket_path(paths: &AppPaths) -> PathBuf {
    paths.cache_dir.join("daemon.sock")
}

#[cfg(unix)]
fn empty_daemon_listener() -> DaemonSearchListener {
    None
}

#[cfg(not(unix))]
fn empty_daemon_listener() -> DaemonSearchListener {
    DaemonSearchListener
}

#[cfg(unix)]
fn prepare_daemon_socket(paths: &AppPaths) -> Result<DaemonSearchListener> {
    use std::os::unix::net::{UnixListener, UnixStream};

    fs::create_dir_all(&paths.cache_dir).map_err(|source| {
        app_err(AppError::InspectDaemonWatchPath {
            path: paths.cache_dir.clone(),
            source,
        })
    })?;
    let socket = daemon_socket_path(paths);
    if socket.exists() {
        if UnixStream::connect(&socket).is_ok() {
            return Err(app_err(AppError::DaemonAlreadyRunning {
                socket: socket.clone(),
            }));
        }

        fs::remove_file(&socket).map_err(|source| {
            app_err(AppError::InspectDaemonWatchPath {
                path: socket.clone(),
                source,
            })
        })?;
    }

    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(source) if source.kind() == std::io::ErrorKind::PermissionDenied => return Ok(None),
        Err(source) => {
            return Err(app_err(AppError::InspectDaemonWatchPath {
                path: socket.clone(),
                source,
            }));
        }
    };
    listener.set_nonblocking(true).map_err(|source| {
        app_err(AppError::InspectDaemonWatchPath {
            path: socket,
            source,
        })
    })?;
    Ok(Some(listener))
}

#[cfg(not(unix))]
fn prepare_daemon_socket(_paths: &AppPaths) -> Result<DaemonSearchListener> {
    Ok(DaemonSearchListener)
}

#[cfg(unix)]
fn daemon_search(paths: &AppPaths, query: &str, limit: usize) -> Result<Option<Vec<SearchResult>>> {
    let response = daemon_request(
        paths,
        DaemonSearchRequest {
            query: query.to_string(),
            limit,
            dry_run: false,
        },
        DAEMON_SEARCH_TIMEOUT,
    )?;
    Ok(response.map(|response| response.results))
}

#[cfg(unix)]
fn daemon_dry_run(paths: &AppPaths, query: &str, limit: usize) -> Result<Option<SearchDryRun>> {
    let Some(response) = daemon_request(
        paths,
        DaemonSearchRequest {
            query: query.to_string(),
            limit,
            dry_run: true,
        },
        DAEMON_DRY_RUN_TIMEOUT,
    )?
    else {
        return Ok(None);
    };
    response
        .dry_run
        .map(Some)
        .ok_or_else(|| app_err(AppError::DaemonDryRunMissing))
}

#[cfg(unix)]
fn daemon_request(
    paths: &AppPaths,
    request: DaemonSearchRequest,
    timeout: Duration,
) -> Result<Option<DaemonSearchResponse>> {
    use std::os::unix::net::UnixStream;

    let socket = daemon_socket_path(paths);
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return Ok(None);
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = serde_json::to_vec(&request)
        .map_err(|source| app_err(AppError::SerializeDaemonMessage { source }))?;
    if let Err(source) = stream.write_all(&request) {
        if is_daemon_timeout(&source) {
            return Ok(None);
        }
        return Err(app_err(AppError::DaemonSocketIo { source }));
    }
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|source| app_err(AppError::DaemonSocketIo { source }))?;

    let mut response = Vec::new();
    if let Err(source) = stream.read_to_end(&mut response) {
        if is_daemon_timeout(&source) {
            return Ok(None);
        }
        return Err(app_err(AppError::DaemonSocketIo { source }));
    }
    let response = serde_json::from_slice::<DaemonSearchResponse>(&response)
        .map_err(|source| app_err(AppError::ParseDaemonMessage { source }))?;
    Ok(Some(response))
}

fn is_daemon_timeout(source: &std::io::Error) -> bool {
    matches!(
        source.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

#[cfg(not(unix))]
fn daemon_search(
    _paths: &AppPaths,
    _query: &str,
    _limit: usize,
) -> Result<Option<Vec<SearchResult>>> {
    Ok(None)
}

#[cfg(not(unix))]
fn daemon_dry_run(_paths: &AppPaths, _query: &str, _limit: usize) -> Result<Option<SearchDryRun>> {
    Ok(None)
}

#[cfg(unix)]
async fn handle_daemon_search_requests<E>(
    listener: &DaemonSearchListener,
    paths: &AppPaths,
    database: &Database,
    embedder: &E,
    search_cache: &mut Option<SearchCache>,
) -> Result<()>
where
    E: Embedder,
{
    let Some(listener) = listener else {
        return Ok(());
    };

    loop {
        let (mut stream, _) = match listener.accept() {
            Ok(connection) => connection,
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(source) => return Err(app_err(AppError::DaemonSocketIo { source })),
        };
        stream
            .set_nonblocking(false)
            .map_err(|source| app_err(AppError::DaemonSocketIo { source }))?;

        let mut request = Vec::new();
        if let Err(source) = stream.read_to_end(&mut request) {
            if is_daemon_client_disconnect(&source) {
                continue;
            }
            return Err(app_err(AppError::DaemonSocketIo { source }));
        }
        let request = match serde_json::from_slice::<DaemonSearchRequest>(&request) {
            Ok(request) => request,
            Err(_) => continue,
        };
        let settings = Settings::load_or_create(&paths.config_file)?;
        let generic_terms = search_generic_terms(&settings);
        let (cache, cache_status) =
            search_cache_for(database, search_cache, &generic_terms).await?;
        let searcher = Searcher::new_with_settings(database, embedder, &settings);
        let response = if request.dry_run {
            let report = searcher
                .dry_run_with_cache_status(&request.query, request.limit, cache, cache_status)
                .await?;
            DaemonSearchResponse {
                results: report.results.clone(),
                dry_run: Some(report),
            }
        } else {
            let results = searcher
                .search_with_cache(&request.query, request.limit, cache)
                .await?;
            DaemonSearchResponse {
                results,
                dry_run: None,
            }
        };
        let response = serde_json::to_vec(&response)
            .map_err(|source| app_err(AppError::SerializeDaemonMessage { source }))?;
        if let Err(source) = stream.write_all(&response) {
            if is_daemon_client_disconnect(&source) {
                continue;
            }
            return Err(app_err(AppError::DaemonSocketIo { source }));
        }
    }
}

async fn search_cache_for<'a>(
    database: &Database,
    search_cache: &'a mut Option<SearchCache>,
    generic_terms: &HashSet<String>,
) -> Result<(&'a SearchCache, CacheDryRunStatus)> {
    let revision = database.current_revision().await?;
    let status = if search_cache
        .as_ref()
        .is_none_or(|cache| !cache.matches_revision_and_generic_terms(revision, generic_terms))
    {
        *search_cache = Some(SearchCache::load(database, generic_terms).await?);
        CacheDryRunStatus::Miss
    } else {
        CacheDryRunStatus::Hit
    };

    Ok((
        search_cache
            .as_ref()
            .expect("search cache should be populated"),
        status,
    ))
}

fn search_generic_terms(settings: &Settings) -> HashSet<String> {
    settings
        .index
        .generic_terms
        .iter()
        .map(|term| term.to_ascii_lowercase())
        .collect()
}

fn is_daemon_client_disconnect(source: &std::io::Error) -> bool {
    matches!(
        source.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::NotConnected
    )
}

#[cfg(not(unix))]
async fn handle_daemon_search_requests<E>(
    _listener: &DaemonSearchListener,
    _paths: &AppPaths,
    _database: &Database,
    _embedder: &E,
    _search_cache: &mut Option<SearchCache>,
) -> Result<()>
where
    E: Embedder,
{
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RootFileSnapshot {
    files: HashMap<PathBuf, FileSnapshot>,
}

impl RootFileSnapshot {
    fn diff(&self, next: &Self) -> FileSnapshotDiff {
        let mut changed_files = Vec::new();
        let mut deleted_files = Vec::new();

        for (path, snapshot) in &next.files {
            if self.files.get(path) != Some(snapshot) {
                changed_files.push(path.clone());
            }
        }

        for path in self.files.keys() {
            if !next.files.contains_key(path) {
                deleted_files.push(path.to_string_lossy().into_owned());
            }
        }

        changed_files.sort();
        deleted_files.sort();

        FileSnapshotDiff {
            changed_files,
            deleted_files,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileSnapshot {
    size_bytes: u64,
    modified_unix_nanos: u128,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct FileSnapshotDiff {
    changed_files: Vec<PathBuf>,
    deleted_files: Vec<String>,
}

fn daemon_root_file_snapshot(root: &Path, settings: &Settings) -> Result<RootFileSnapshot> {
    let mut snapshot = RootFileSnapshot::default();
    collect_file_snapshots(
        root,
        settings,
        &mut snapshot,
        DaemonSnapshotDepth::IndexRoot,
    )?;
    Ok(snapshot)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonSnapshotDepth {
    IndexRoot,
    TopLevelDirectory { depth: usize },
}

impl DaemonSnapshotDepth {
    fn child_position(self) -> Option<Self> {
        match self {
            Self::IndexRoot => Some(Self::TopLevelDirectory { depth: 0 }),
            Self::TopLevelDirectory { depth } => depth
                .checked_add(1)
                .map(|depth| Self::TopLevelDirectory { depth }),
        }
    }

    fn exceeds_max_depth(self, settings: &Settings) -> bool {
        match self {
            Self::IndexRoot => false,
            Self::TopLevelDirectory { depth } => {
                depth > settings.index.max_depth_per_top_level_directory
            }
        }
    }
}

fn collect_file_snapshots(
    path: &Path,
    settings: &Settings,
    snapshot: &mut RootFileSnapshot,
    depth: DaemonSnapshotDepth,
) -> Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(source) => {
            return Err(app_err(AppError::InspectDaemonWatchPath {
                path: path.to_path_buf(),
                source,
            }));
        }
    };

    if !metadata.is_dir() {
        snapshot.files.insert(
            path.to_path_buf(),
            FileSnapshot {
                size_bytes: metadata.len(),
                modified_unix_nanos: metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_nanos())
                    .unwrap_or(0),
            },
        );
        return Ok(());
    }

    let mut entries = fs::read_dir(path)
        .map_err(|source| {
            app_err(AppError::InspectDaemonWatchPath {
                path: path.to_path_buf(),
                source,
            })
        })?
        .filter_map(|entry| entry.ok())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let file_type = entry.file_type().map_err(|source| {
            app_err(AppError::InspectDaemonWatchPath {
                path: path.clone(),
                source,
            })
        })?;

        if file_type.is_dir() {
            if settings.index.is_excluded_directory_name(&name) {
                continue;
            }
            let Some(child_depth) = depth.child_position() else {
                continue;
            };
            if child_depth.exceeds_max_depth(settings) {
                continue;
            }
            collect_file_snapshots(&path, settings, snapshot, child_depth)?;
        } else if settings.index.is_excluded_name(&name) {
            continue;
        } else {
            collect_file_snapshots(&path, settings, snapshot, depth)?;
        }
    }

    Ok(())
}

pub async fn resolve_cd_script(args: Vec<OsString>) -> Vec<u8> {
    if is_explicit_cds_command(&args) {
        return crate::emit_cds_command_script(&args);
    }

    if let Some(query) = implied_search_query(&args)
        && let Ok(results) = search_text(&query, 5).await
        && let Some(result) = results
            .into_iter()
            .find(|result| Path::new(&result.path).is_dir())
    {
        return crate::emit_cd_script(&[OsString::from(result.path)]);
    }

    crate::emit_cd_script(&args)
}

fn is_explicit_cds_command(args: &[OsString]) -> bool {
    let Some(first) = args.first().and_then(|arg| arg.to_str()) else {
        return false;
    };

    matches!(
        first,
        "--daemon"
            | "--dir-type-count"
            | "--dry-run"
            | "--help"
            | "--index"
            | "--init"
            | "--reset"
            | "--restart-daemon"
            | "--search"
            | "--shell-init"
            | "--version"
            | "-h"
            | "-V"
    )
}

pub fn implied_search_query(args: &[OsString]) -> Option<String> {
    semantic_query(args)
}

fn join_query(query: Vec<OsString>) -> Result<String> {
    let mut parts = Vec::with_capacity(query.len());

    for part in query {
        let part = part.to_str().ok_or_else(|| {
            app_err(AppError::InvalidSearchQueryUtf8 {
                query: part.clone(),
            })
        })?;
        parts.push(part.to_string());
    }

    Ok(parts.join(" "))
}

fn semantic_query(args: &[OsString]) -> Option<String> {
    semantic_query_in(args, current_shell_directory().as_deref())
}

fn semantic_query_in(args: &[OsString], directory: Option<&Path>) -> Option<String> {
    if args.is_empty() {
        return None;
    }

    let mut parts = Vec::with_capacity(args.len());
    for arg in args {
        let arg = arg.to_str()?;
        if is_cd_syntax(arg) {
            return None;
        }
        parts.push(arg);
    }

    if args.len() == 1 && local_entry_exists(parts[0], directory) {
        return None;
    }

    let query = parts.join(" ");
    query.chars().find(|ch| !ch.is_whitespace())?;
    Some(query)
}

fn is_cd_syntax(arg: &str) -> bool {
    arg.is_empty()
        || arg == "-"
        || arg == "--"
        || arg == "."
        || arg == ".."
        || arg == "~"
        || arg.starts_with('-')
        || arg.starts_with("~/")
        || arg.contains('/')
}

fn local_entry_exists(name: &str, directory: Option<&Path>) -> bool {
    let Some(directory) = directory else {
        return true;
    };

    directory.join(name).exists()
}

fn current_shell_directory() -> Option<PathBuf> {
    env::var_os("PWD")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| env::current_dir().ok())
}

enum RuntimeEmbedder {
    #[cfg(feature = "real-embedder")]
    Bge(Box<BgeSmallEmbedder>),
    Fake(FakeEmbedder),
}

impl RuntimeEmbedder {
    fn load(paths: &AppPaths) -> Result<Self> {
        if env::var("CDS_EMBEDDER").is_ok_and(|value| value.eq_ignore_ascii_case("fake")) {
            return Ok(Self::Fake(FakeEmbedder::default()));
        }

        load_real_embedder(paths)
    }
}

impl Embedder for RuntimeEmbedder {
    fn dimensions(&self) -> usize {
        match self {
            #[cfg(feature = "real-embedder")]
            Self::Bge(embedder) => embedder.dimensions(),
            Self::Fake(embedder) => embedder.dimensions(),
        }
    }

    fn embed(&self, text: &str) -> crate::embed::Result<Vec<f32>> {
        match self {
            #[cfg(feature = "real-embedder")]
            Self::Bge(embedder) => embedder.embed(text),
            Self::Fake(embedder) => embedder.embed(text),
        }
    }

    fn embed_document(&self, text: &str) -> crate::embed::Result<Vec<f32>> {
        match self {
            #[cfg(feature = "real-embedder")]
            Self::Bge(embedder) => embedder.embed_document(text),
            Self::Fake(embedder) => embedder.embed_document(text),
        }
    }

    fn embed_query(&self, text: &str) -> crate::embed::Result<Vec<f32>> {
        match self {
            #[cfg(feature = "real-embedder")]
            Self::Bge(embedder) => embedder.embed_query(text),
            Self::Fake(embedder) => embedder.embed_query(text),
        }
    }
}

#[cfg(feature = "real-embedder")]
fn load_real_embedder(paths: &AppPaths) -> Result<RuntimeEmbedder> {
    BgeSmallEmbedder::new(&paths.cache_dir)
        .map(Box::new)
        .map(RuntimeEmbedder::Bge)
        .map_err(embed_err)
}

#[cfg(not(feature = "real-embedder"))]
fn load_real_embedder(_paths: &AppPaths) -> Result<RuntimeEmbedder> {
    Err(embed_err(crate::embed::EmbedError::Model {
        message: "real embedder support is disabled; rebuild with the real-embedder feature or set CDS_EMBEDDER=fake".to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    #[test]
    fn semantic_query_is_allowed_when_no_exact_local_entry_matches() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();

        assert_eq!(
            semantic_query_in(&[os("Projects")], Some(temp.path())),
            Some("Projects".to_string())
        );
    }

    #[test]
    fn semantic_query_is_allowed_when_only_local_prefix_matches() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("playground")).unwrap();

        assert_eq!(
            semantic_query_in(&[os("Projects")], Some(temp.path())),
            Some("Projects".to_string())
        );
    }

    #[test]
    fn semantic_query_is_blocked_when_exact_local_entry_matches() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("Projects")).unwrap();

        assert_eq!(
            semantic_query_in(&[os("Projects")], Some(temp.path())),
            None
        );
    }

    #[test]
    fn semantic_query_is_blocked_for_cd_syntax() {
        let temp = tempfile::tempdir().unwrap();

        assert_eq!(
            semantic_query_in(&[os("-P"), os("Projects")], Some(temp.path())),
            None
        );
        assert_eq!(
            semantic_query_in(&[os("--"), os("-dir")], Some(temp.path())),
            None
        );
        assert_eq!(
            semantic_query_in(&[os("../Projects")], Some(temp.path())),
            None
        );
        assert_eq!(semantic_query_in(&[os("~")], Some(temp.path())), None);
    }

    #[test]
    fn daemon_client_disconnect_errors_are_non_fatal() {
        for kind in [
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::NotConnected,
        ] {
            assert!(is_daemon_client_disconnect(&std::io::Error::from(kind)));
        }
    }

    #[tokio::test]
    async fn search_cache_misses_after_database_revision_changes() {
        use crate::db::{DocumentKind, IndexedDocument};

        let database = Database::open_in_memory().await.unwrap();
        let generic_terms = HashSet::new();
        let mut search_cache = None;

        let first_status = {
            let (_, status) = search_cache_for(&database, &mut search_cache, &generic_terms)
                .await
                .unwrap();
            status
        };
        let second_status = {
            let (_, status) = search_cache_for(&database, &mut search_cache, &generic_terms)
                .await
                .unwrap();
            status
        };
        assert_eq!(first_status, CacheDryRunStatus::Miss);
        assert_eq!(second_status, CacheDryRunStatus::Hit);

        database
            .upsert_document(&IndexedDocument {
                path: "/tmp/project".to_string(),
                name: "project".to_string(),
                kind: DocumentKind::Directory,
                parent_path: Some("/tmp".to_string()),
                searchable_text: "project readme cargo".to_string(),
                embedding: vec![0.1, 0.2, 0.3],
                metadata_fingerprint: "fingerprint".to_string(),
                size_bytes: 4096,
                created_unix_seconds: Some(10),
                modified_unix_seconds: 12,
                accessed_unix_seconds: Some(14),
                readonly: false,
                indexed_unix_seconds: 34,
            })
            .await
            .unwrap();

        let stale_status = {
            let (_, status) = search_cache_for(&database, &mut search_cache, &generic_terms)
                .await
                .unwrap();
            status
        };
        assert_eq!(stale_status, CacheDryRunStatus::Miss);
    }

    #[test]
    fn semantic_query_joins_plain_words() {
        let temp = tempfile::tempdir().unwrap();

        assert_eq!(
            semantic_query_in(&[os("chrome"), os("extension")], Some(temp.path())),
            Some("chrome extension".to_string())
        );
    }

    #[test]
    fn implied_search_query_uses_cd_semantic_rules() {
        assert_eq!(implied_search_query(&[os("-P"), os("Projects")]), None);
        assert_eq!(implied_search_query(&[os("../Projects")]), None);
        assert_eq!(implied_search_query(&[]), None);
    }

    #[tokio::test]
    async fn explicit_cds_commands_are_emitted_as_commands() {
        assert_eq!(
            resolve_cd_script(vec![os("--restart-daemon")]).await,
            b"command cds '--restart-daemon'\n"
        );
        assert_eq!(
            resolve_cd_script(vec![os("--reset")]).await,
            b"command cds '--reset'\n"
        );
        assert_eq!(
            resolve_cd_script(vec![os("--dry-run"), os("github"), os("clone")]).await,
            b"command cds '--dry-run' 'github' 'clone'\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn detects_cds_process_commands() {
        assert!(is_cds_process_command("/Users/me/.cargo/bin/cds"));
        assert!(is_cds_process_command("target/debug/cds"));
        assert!(!is_cds_process_command("/Users/me/.cargo/bin/cds-helper"));
        assert!(!is_cds_process_command("bash"));

        let pid = cds_pid_from_process_line(
            "  123   501   /Users/me/.cargo/bin/cds --daemon",
            Some(501),
            999,
        );
        assert_eq!(pid, Some(123));
        let restart = cds_pid_from_process_line(
            " 124 501 /Users/me/.cargo/bin/cds --restart-daemon",
            Some(501),
            999,
        );
        assert_eq!(restart, Some(124));
        let search = cds_pid_from_process_line(
            " 125 501 /Users/me/.cargo/bin/cds --search daemon",
            Some(501),
            999,
        );
        assert_eq!(search, Some(125));
        let current_process = cds_pid_from_process_line(
            " 999 501 /Users/me/.cargo/bin/cds --restart-daemon",
            Some(501),
            999,
        );
        assert_eq!(current_process, None);
        let other_user =
            cds_pid_from_process_line(" 123 502 /Users/me/.cargo/bin/cds --daemon", Some(501), 999);
        assert_eq!(other_user, None);
    }

    #[test]
    fn file_snapshots_diff_changed_and_deleted_files() {
        let unchanged = PathBuf::from("/tmp/project/README.md");
        let changed = PathBuf::from("/tmp/project/package.json");
        let deleted = PathBuf::from("/tmp/project/Cargo.toml");

        let previous = RootFileSnapshot {
            files: HashMap::from([
                (
                    unchanged.clone(),
                    FileSnapshot {
                        size_bytes: 10,
                        modified_unix_nanos: 10,
                    },
                ),
                (
                    changed.clone(),
                    FileSnapshot {
                        size_bytes: 20,
                        modified_unix_nanos: 20,
                    },
                ),
                (
                    deleted.clone(),
                    FileSnapshot {
                        size_bytes: 30,
                        modified_unix_nanos: 30,
                    },
                ),
            ]),
        };
        let next = RootFileSnapshot {
            files: HashMap::from([
                (
                    unchanged,
                    FileSnapshot {
                        size_bytes: 10,
                        modified_unix_nanos: 10,
                    },
                ),
                (
                    changed.clone(),
                    FileSnapshot {
                        size_bytes: 20,
                        modified_unix_nanos: 21,
                    },
                ),
            ]),
        };

        let diff = previous.diff(&next);

        assert_eq!(diff.changed_files, vec![changed]);
        assert_eq!(
            diff.deleted_files,
            vec![deleted.to_string_lossy().into_owned()]
        );
    }

    #[test]
    fn daemon_file_snapshot_respects_recursive_depth_limit() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let included = root.join("top").join("allowed").join("README.md");
        let excluded = root
            .join("top")
            .join("allowed")
            .join("too-deep")
            .join("README.md");
        std::fs::create_dir_all(included.parent().unwrap()).unwrap();
        std::fs::create_dir_all(excluded.parent().unwrap()).unwrap();
        std::fs::write(&included, "included content").unwrap();
        std::fs::write(&excluded, "excluded content").unwrap();

        let mut settings = Settings::default();
        settings.index.max_depth_per_top_level_directory = 1;

        let snapshot = daemon_root_file_snapshot(&root, &settings).unwrap();

        assert!(snapshot.files.contains_key(&included));
        assert!(!snapshot.files.contains_key(&excluded));
    }
}
