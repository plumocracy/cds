use std::ffi::{OsStr, OsString};

pub mod app;
pub mod cli;
pub mod config;
pub mod db;
pub mod embed;
pub mod error;
pub mod index;
pub mod search;

pub fn emit_cd_script(args: &[OsString]) -> Vec<u8> {
    let request = CdRequest::new(args.to_vec());
    request.to_shell_script()
}

pub fn emit_cds_command_script(args: &[OsString]) -> Vec<u8> {
    let mut script = b"command cds".to_vec();
    for arg in args {
        script.push(b' ');
        script.extend(shell_quote(arg));
    }
    script.push(b'\n');
    script
}

pub fn shell_init(shell: cli::Shell) -> &'static str {
    match shell {
        cli::Shell::Bash => BASH_ZSH_INIT,
        cli::Shell::Zsh => BASH_ZSH_INIT,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CdRequest {
    args: Vec<OsString>,
}

impl CdRequest {
    fn new(args: Vec<OsString>) -> Self {
        Self { args }
    }

    fn to_shell_script(&self) -> Vec<u8> {
        let mut script = b"builtin cd".to_vec();
        for arg in &self.args {
            script.push(b' ');
            script.extend(shell_quote(arg));
        }
        script.push(b'\n');
        script
    }
}

#[cfg(unix)]
fn shell_quote(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;

    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return b"''".to_vec();
    }

    let mut quoted = Vec::with_capacity(bytes.len() + 2);
    quoted.push(b'\'');
    for byte in bytes {
        if *byte == b'\'' {
            quoted.extend(b"'\\''");
        } else {
            quoted.push(*byte);
        }
    }
    quoted.push(b'\'');
    quoted
}

#[cfg(not(unix))]
fn shell_quote(value: &OsStr) -> Vec<u8> {
    let value = value.to_string_lossy();
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted.into_bytes()
}

const BASH_ZSH_INIT: &str = r#"cds() {
    local __cds_script
    local __cds_status

    case "${1-}" in
        --daemon|--dir-type-count|--init|--index|--reset|--restart-daemon|--search|--help|-h|--version|-V|--shell-init)
            command cds "$@"
            return $?
            ;;
    esac

    __cds_script="$(command cds --cds-emit -- "$@")"
    __cds_status=$?
    if [ "$__cds_status" -ne 0 ]; then
        return "$__cds_status"
    fi

    eval "$__cds_script"
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    #[test]
    fn emits_plain_cd_for_no_arguments() {
        assert_eq!(emit_cd_script(&[]), b"builtin cd\n");
    }

    #[test]
    fn preserves_cd_flags_and_operands() {
        let script = emit_cd_script(&[os("-P"), os("../some path")]);
        assert_eq!(script, b"builtin cd '-P' '../some path'\n");
    }

    #[test]
    fn emits_cds_command_script() {
        let script = emit_cds_command_script(&[os("--restart-daemon")]);
        assert_eq!(script, b"command cds '--restart-daemon'\n");
    }

    #[test]
    fn preserves_double_dash() {
        let script = emit_cd_script(&[os("--"), os("-directory")]);
        assert_eq!(script, b"builtin cd '--' '-directory'\n");
    }

    #[test]
    fn quotes_single_quotes_safely() {
        let script = emit_cd_script(&[os("team's app")]);
        assert_eq!(script, b"builtin cd 'team'\\''s app'\n");
    }
}
