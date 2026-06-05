use std::env;
use std::ffi::{OsStr, OsString};

use clap::builder::OsStringValueParser;
use clap::{Arg, ArgAction, ArgGroup, Command, error::ErrorKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    Daemon { run_once: bool },
    DirectoryTypeCount,
    DryRun { query: Vec<OsString> },
    EmitCd { args: Vec<OsString> },
    Index { roots: Vec<OsString> },
    Init,
    InitConfig,
    Reset,
    RestartDaemon,
    Search { query: Vec<OsString> },
    ShellInit { shell: Shell },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
}

impl Shell {
    pub fn parse(value: Option<&OsStr>) -> Self {
        let Some(value) = value else {
            return Self::from_env();
        };

        let name = value.to_string_lossy();
        let name = name.rsplit('/').next().unwrap_or(&name);

        match name {
            "bash" => Self::Bash,
            "zsh" => Self::Zsh,
            _ => Self::Zsh,
        }
    }

    fn from_env() -> Self {
        let shell = env::var_os("SHELL");
        Self::parse(shell.as_deref())
    }
}

pub fn parse_invocation(
    args: impl IntoIterator<Item = OsString>,
) -> Result<Invocation, clap::Error> {
    let mut argv = vec![OsString::from("cds")];
    argv.extend(args);

    let matches = command().try_get_matches_from(argv)?;

    if matches.contains_id("emit-cd") {
        let mut args = matches
            .get_many::<OsString>("emit-cd")
            .map(|args| args.cloned().collect())
            .unwrap_or_default();
        strip_internal_separator(&mut args);
        return Ok(Invocation::EmitCd { args });
    }

    if matches.get_flag("daemon") {
        return Ok(Invocation::Daemon { run_once: false });
    }

    if matches.get_flag("daemon-once") {
        return Ok(Invocation::Daemon { run_once: true });
    }

    if matches.get_flag("dir-type-count") {
        return Ok(Invocation::DirectoryTypeCount);
    }

    if matches.contains_id("dry-run") {
        let query = matches
            .get_many::<OsString>("dry-run")
            .map(|query| query.cloned().collect())
            .unwrap_or_default();
        return Ok(Invocation::DryRun { query });
    }

    if matches.get_flag("init") {
        return Ok(Invocation::Init);
    }

    if matches.get_flag("init-config") {
        return Ok(Invocation::InitConfig);
    }

    if matches.get_flag("reset") {
        return Ok(Invocation::Reset);
    }

    if matches.get_flag("restart-daemon") {
        return Ok(Invocation::RestartDaemon);
    }

    if matches.contains_id("index") {
        let roots = matches
            .get_many::<OsString>("index")
            .map(|roots| roots.cloned().collect())
            .unwrap_or_default();
        return Ok(Invocation::Index { roots });
    }

    if matches.contains_id("search") {
        let query = matches
            .get_many::<OsString>("search")
            .map(|query| query.cloned().collect())
            .unwrap_or_default();
        return Ok(Invocation::Search { query });
    }

    if matches.contains_id("shell-init") {
        let shell = matches
            .get_one::<OsString>("shell-init")
            .and_then(|shell| (!shell.is_empty()).then_some(shell.as_os_str()));
        return Ok(Invocation::ShellInit {
            shell: Shell::parse(shell),
        });
    }

    Err(command().error(ErrorKind::MissingSubcommand, direct_invocation_error()))
}

pub fn command() -> Command {
    Command::new("cds")
        .about("cd with semantic search")
        .version(clap::crate_version!())
        .disable_help_subcommand(true)
        .group(ArgGroup::new("mode").args([
            "daemon",
            "daemon-once",
            "dir-type-count",
            "dry-run",
            "emit-cd",
            "index",
            "init",
            "init-config",
            "reset",
            "restart-daemon",
            "search",
            "shell-init",
        ]))
        .arg(
            Arg::new("daemon")
                .long("daemon")
                .help("Run the cds indexing daemon in the foreground")
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("daemon-once")
                .long("daemon-once")
                .hide(true)
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("init")
                .long("init")
                .help("Create the default config and database, then index configured roots")
                .conflicts_with_all([
                    "dir-type-count",
                    "dry-run",
                    "emit-cd",
                    "index",
                    "init-config",
                    "reset",
                    "restart-daemon",
                    "search",
                    "shell-init",
                ])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("init-config")
                .long("init-config")
                .help("Create the default config without creating the database or indexing")
                .conflicts_with_all([
                    "daemon",
                    "daemon-once",
                    "dir-type-count",
                    "dry-run",
                    "emit-cd",
                    "index",
                    "init",
                    "reset",
                    "restart-daemon",
                    "search",
                    "shell-init",
                ])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("dir-type-count")
                .long("dir-type-count")
                .help("Print detected directory type counts")
                .conflicts_with_all([
                    "dry-run",
                    "emit-cd",
                    "index",
                    "init",
                    "init-config",
                    "reset",
                    "search",
                    "shell-init",
                ])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("reset")
                .long("reset")
                .help("Delete all indexed data from the cds database")
                .conflicts_with_all([
                    "dir-type-count",
                    "dry-run",
                    "emit-cd",
                    "index",
                    "init",
                    "init-config",
                    "restart-daemon",
                    "search",
                    "shell-init",
                ])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("restart-daemon")
                .long("restart-daemon")
                .help("Kill existing cds daemons and start a fresh one")
                .conflicts_with_all([
                    "daemon",
                    "daemon-once",
                    "dir-type-count",
                    "dry-run",
                    "emit-cd",
                    "index",
                    "init",
                    "init-config",
                    "reset",
                    "search",
                    "shell-init",
                ])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("shell-init")
                .long("shell-init")
                .value_name("SHELL")
                .help("Print shell integration for the current shell")
                .long_help(
                    "Print shell integration for the current shell. Pass `bash` or `zsh` \
                     explicitly, or omit the value to infer it from $SHELL.",
                )
                .action(ArgAction::Set)
                .num_args(0..=1)
                .default_missing_value("")
                .conflicts_with_all([
                    "dir-type-count",
                    "dry-run",
                    "emit-cd",
                    "index",
                    "init",
                    "init-config",
                    "reset",
                    "restart-daemon",
                    "search",
                ])
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("index")
                .long("index")
                .value_name("ROOT")
                .help("Index configured roots or explicit paths")
                .long_help(
                    "Index configured roots, or pass one or more ROOT values to index explicit \
                     paths instead of the configured roots.",
                )
                .num_args(0..)
                .conflicts_with_all([
                    "dir-type-count",
                    "dry-run",
                    "emit-cd",
                    "init-config",
                    "init",
                    "reset",
                    "restart-daemon",
                    "search",
                    "shell-init",
                ])
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("dry-run")
                .long("dry-run")
                .value_name("QUERY")
                .help("Print SQL candidates, embedding scores, and the winning directory")
                .num_args(1..)
                .conflicts_with_all([
                    "dir-type-count",
                    "emit-cd",
                    "index",
                    "init",
                    "init-config",
                    "reset",
                    "restart-daemon",
                    "search",
                    "shell-init",
                ])
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("search")
                .long("search")
                .value_name("QUERY")
                .help("Search indexed directories")
                .num_args(1..)
                .conflicts_with_all([
                    "dir-type-count",
                    "dry-run",
                    "emit-cd",
                    "index",
                    "init",
                    "reset",
                    "restart-daemon",
                    "shell-init",
                ])
                .value_parser(OsStringValueParser::new()),
        )
        .arg(
            Arg::new("emit-cd")
                .long("cds-emit")
                .hide(true)
                .num_args(0..)
                .trailing_var_arg(true)
                .allow_hyphen_values(true)
                .conflicts_with_all([
                    "dir-type-count",
                    "dry-run",
                    "index",
                    "init",
                    "init-config",
                    "reset",
                    "search",
                    "shell-init",
                ])
                .value_parser(OsStringValueParser::new()),
        )
}

fn direct_invocation_error() -> &'static str {
    "install the shell integration first: eval \"$(command cds --shell-init zsh)\""
}

fn strip_internal_separator(args: &mut Vec<OsString>) {
    if args.first().is_some_and(|arg| arg == "--") {
        args.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    #[test]
    fn parses_internal_emit_separator() {
        let parsed = parse_invocation([os("--cds-emit"), os("--"), os("-")]).unwrap();
        assert_eq!(
            parsed,
            Invocation::EmitCd {
                args: vec![os("-")]
            }
        );
    }

    #[test]
    fn preserves_emit_args_that_look_like_options() {
        let parsed =
            parse_invocation([os("--cds-emit"), os("--"), os("-P"), os("--"), os("-dir")]).unwrap();
        assert_eq!(
            parsed,
            Invocation::EmitCd {
                args: vec![os("-P"), os("--"), os("-dir")]
            }
        );
    }

    #[test]
    fn parses_shell_init() {
        let parsed = parse_invocation([os("--shell-init"), os("bash")]).unwrap();
        assert_eq!(parsed, Invocation::ShellInit { shell: Shell::Bash });
    }

    #[test]
    fn parses_init() {
        assert_eq!(parse_invocation([os("--init")]).unwrap(), Invocation::Init);
    }

    #[test]
    fn parses_init_config() {
        assert_eq!(
            parse_invocation([os("--init-config")]).unwrap(),
            Invocation::InitConfig
        );
    }

    #[test]
    fn parses_directory_type_count() {
        assert_eq!(
            parse_invocation([os("--dir-type-count")]).unwrap(),
            Invocation::DirectoryTypeCount
        );
    }

    #[test]
    fn parses_reset() {
        assert_eq!(
            parse_invocation([os("--reset")]).unwrap(),
            Invocation::Reset
        );
    }

    #[test]
    fn parses_restart_daemon() {
        assert_eq!(
            parse_invocation([os("--restart-daemon")]).unwrap(),
            Invocation::RestartDaemon
        );
    }

    #[test]
    fn parses_daemon() {
        assert_eq!(
            parse_invocation([os("--daemon")]).unwrap(),
            Invocation::Daemon { run_once: false }
        );
    }

    #[test]
    fn parses_daemon_once() {
        assert_eq!(
            parse_invocation([os("--daemon-once")]).unwrap(),
            Invocation::Daemon { run_once: true }
        );
    }

    #[test]
    fn init_word_is_cd_input_inside_hidden_emit() {
        assert_eq!(
            parse_invocation([os("--cds-emit"), os("--"), os("init")]).unwrap(),
            Invocation::EmitCd {
                args: vec![os("init")]
            }
        );
    }

    #[test]
    fn index_word_is_cd_input_inside_hidden_emit() {
        assert_eq!(
            parse_invocation([os("--cds-emit"), os("--"), os("index")]).unwrap(),
            Invocation::EmitCd {
                args: vec![os("index")]
            }
        );
    }

    #[test]
    fn parses_index_roots() {
        assert_eq!(
            parse_invocation([os("--index"), os("~/Projects"), os("/tmp/work")]).unwrap(),
            Invocation::Index {
                roots: vec![os("~/Projects"), os("/tmp/work")]
            }
        );
    }

    #[test]
    fn parses_index_without_roots() {
        assert_eq!(
            parse_invocation([os("--index")]).unwrap(),
            Invocation::Index { roots: Vec::new() }
        );
    }

    #[test]
    fn parses_search_query() {
        assert_eq!(
            parse_invocation([os("--search"), os("chrome"), os("extension")]).unwrap(),
            Invocation::Search {
                query: vec![os("chrome"), os("extension")]
            }
        );
    }

    #[test]
    fn parses_dry_run_query() {
        assert_eq!(
            parse_invocation([os("--dry-run"), os("github"), os("clone")]).unwrap(),
            Invocation::DryRun {
                query: vec![os("github"), os("clone")]
            }
        );
    }

    #[test]
    fn search_word_is_cd_input_inside_hidden_emit() {
        assert_eq!(
            parse_invocation([os("--cds-emit"), os("--"), os("search")]).unwrap(),
            Invocation::EmitCd {
                args: vec![os("search")]
            }
        );
    }

    #[test]
    fn infers_shell_when_init_shell_is_omitted() {
        let parsed = parse_invocation([os("--shell-init")]).unwrap();
        assert!(matches!(parsed, Invocation::ShellInit { .. }));
    }
}
