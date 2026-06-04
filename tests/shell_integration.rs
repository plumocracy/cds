use std::path::Path;
use std::process::Command;

#[test]
fn bash_integration_matches_core_cd_behaviors() {
    run_shell_cd_smoke_test("bash", "bash");
}

#[test]
fn zsh_integration_matches_core_cd_behaviors() {
    run_shell_cd_smoke_test("zsh", "zsh");
}

fn run_shell_cd_smoke_test(shell: &str, init_shell: &str) {
    if Command::new(shell)
        .arg("-lc")
        .arg("exit 0")
        .status()
        .is_err()
    {
        eprintln!("skipping {shell} integration test because {shell} is not available");
        return;
    }

    let bin = env!("CARGO_BIN_EXE_cds");
    let bin_dir = Path::new(bin).parent().expect("binary has parent dir");
    let old_path = std::env::var_os("PATH").unwrap_or_default();
    let mut path = bin_dir.as_os_str().to_os_string();
    path.push(":");
    path.push(old_path);

    let script = format!(
        r#"
set -e
eval "$("{bin}" --shell-init {init_shell})"
eval "$(cds --shell-init {init_shell})"

tmp="$(mktemp -d)"
mkdir -p "$tmp/a/b" "$tmp/cpath/d" "$tmp/search" "$tmp/index"
ln -s "$tmp/a" "$tmp/linka"

cd "$tmp"
cds search
[ "$(pwd -P)" = "$(cd "$tmp/search" && pwd -P)" ]

cd "$tmp"
cds index
[ "$(pwd -P)" = "$(cd "$tmp/index" && pwd -P)" ]

cd "$tmp/a"
cds "$tmp/linka/b"
[ "$PWD" = "$tmp/linka/b" ]

cds -P ..
expected="$(cd "$tmp/a" && pwd -P)"
[ "$(pwd -P)" = "$expected" ]

cd "$tmp/linka/b"
cds -L ..
[ "$PWD" = "$tmp/linka" ]

CDPATH="$tmp/cpath" cds d >/dev/null
expected="$(cd "$tmp/cpath/d" && pwd -P)"
[ "$(pwd -P)" = "$expected" ]

cd "$tmp/a"
cd "$tmp/cpath" >/dev/null
cds - >/dev/null
expected="$(cd "$tmp/a" && pwd -P)"
[ "$(pwd -P)" = "$expected" ]

home="$tmp/home"
mkdir -p "$home/Projects/cli-init-test"
printf 'shell integration init project\n' > "$home/Projects/cli-init-test/README.md"
HOME="$home" CDS_CONFIG_DIR="$tmp/config" CDS_DATA_DIR="$tmp/data" CDS_CACHE_DIR="$tmp/cache" cds --init >/tmp/cds-shell-init.out
[ -f "$tmp/config/config.json" ]
[ -f "$tmp/data/cds.sqlite" ]
"#
    );

    let output = Command::new(shell)
        .arg("-lc")
        .arg(script)
        .env("PATH", path)
        .output()
        .expect("shell test starts");

    assert!(
        output.status.success(),
        "{shell} integration failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
