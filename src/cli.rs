use std::env;
use std::ffi::{OsStr, OsString};

use clap::builder::OsStringValueParser;
use clap::{Arg, ArgAction, Command, error::ErrorKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    DirectoryTypeCount,
    EmitCd { args: Vec<OsString> },
    Index { roots: Vec<OsString> },
    Init,
    Reset,
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

    if matches.get_flag("dir-type-count") {
        return Ok(Invocation::DirectoryTypeCount);
    }

    if matches.get_flag("init") {
        return Ok(Invocation::Init);
    }

    if matches.get_flag("reset") {
        return Ok(Invocation::Reset);
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
        .arg(
            Arg::new("init")
                .long("init")
                .help("Create the default config and database, then index configured roots")
                .conflicts_with_all([
                    "dir-type-count",
                    "emit-cd",
                    "index",
                    "reset",
                    "search",
                    "shell-init",
                ])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("dir-type-count")
                .long("dir-type-count")
                .help("Print detected directory type counts")
                .conflicts_with_all(["emit-cd", "index", "init", "reset", "search", "shell-init"])
                .action(ArgAction::SetTrue),
        )
        .arg(
            Arg::new("reset")
                .long("reset")
                .help("Delete all indexed data from the cds database")
                .conflicts_with_all([
                    "dir-type-count",
                    "emit-cd",
                    "index",
                    "init",
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
                    "emit-cd",
                    "index",
                    "init",
                    "reset",
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
                    "emit-cd",
                    "init",
                    "reset",
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
                    "emit-cd",
                    "index",
                    "init",
                    "reset",
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
                    "index",
                    "init",
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
