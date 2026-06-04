use std::env;
use std::ffi::{OsStr, OsString};

use clap::builder::OsStringValueParser;
use clap::{Arg, ArgAction, Command, error::ErrorKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    EmitCd { args: Vec<OsString> },
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

    if let Some(matches) = matches.subcommand_matches("__cds_emit") {
        let args = matches
            .get_many::<OsString>("args")
            .map(|args| args.cloned().collect())
            .unwrap_or_default();
        return Ok(Invocation::EmitCd { args });
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
                .value_parser(OsStringValueParser::new()),
        )
        .subcommand(
            Command::new("__cds_emit").hide(true).arg(
                Arg::new("args")
                    .num_args(0..)
                    .trailing_var_arg(true)
                    .allow_hyphen_values(true)
                    .value_parser(OsStringValueParser::new()),
            ),
        )
}

fn direct_invocation_error() -> &'static str {
    "install the shell integration first: eval \"$(cds --shell-init zsh)\""
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    #[test]
    fn parses_internal_emit_separator() {
        let parsed = parse_invocation([os("__cds_emit"), os("--"), os("-")]).unwrap();
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
            parse_invocation([os("__cds_emit"), os("--"), os("-P"), os("--"), os("-dir")]).unwrap();
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
    fn infers_shell_when_init_shell_is_omitted() {
        let parsed = parse_invocation([os("--shell-init")]).unwrap();
        assert!(matches!(parsed, Invocation::ShellInit { .. }));
    }
}
