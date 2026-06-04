#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
shell_name="$(basename "${SHELL:-}")"
force_install=0

usage() {
	cat <<EOF
Usage: ./install.sh [--force]

Options:
  --force    Pass --force to cargo install so an existing cds binary is replaced.
  -h, --help Show this help message.
EOF
}

while [ "$#" -gt 0 ]; do
	case "$1" in
	--force)
		force_install=1
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "error: unknown option: $1" >&2
		usage >&2
		exit 2
		;;
	esac
	shift
done

if ! command -v cargo >/dev/null 2>&1; then
	echo "error: cargo is required to install cds" >&2
	echo "Install Rust from https://rustup.rs, then rerun ./install.sh" >&2
	exit 1
fi

case "$shell_name" in
zsh)
	profile="${ZDOTDIR:-$HOME}/.zshrc"
	init_shell="zsh"
	;;
bash)
	profile="$HOME/.bashrc"
	init_shell="bash"
	;;
*)
	profile="${ZDOTDIR:-$HOME}/.zshrc"
	init_shell="zsh"
	echo "warning: unsupported shell '${shell_name:-unknown}', defaulting setup to zsh" >&2
	;;
esac

echo "Installing cds with cargo..."
cargo_args=(install --path "$repo_dir")
if [ "$force_install" -eq 1 ]; then
	cargo_args+=(--force)
fi
cargo "${cargo_args[@]}"

cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
cache_dir="${CDS_CACHE_DIR:-$HOME/.cache/cds}"
mkdir -p "$(dirname "$profile")"
touch "$profile"

if ! grep -Fq '# >>> cds init >>>' "$profile"; then
	cat >>"$profile" <<EOF

# >>> cds init >>>
if [ -x "$cargo_bin/cds" ]; then
    eval "\$(command "$cargo_bin/cds" --shell-init $init_shell)"
fi
# <<< cds init <<<
EOF
	echo "Added cds shell integration to $profile"
else
	echo "cds shell integration already exists in $profile"
fi

if ! command -v cds >/dev/null 2>&1; then
	echo "warning: cds is installed at $cargo_bin/cds, but $cargo_bin is not on PATH" >&2
	echo "Add this to your shell profile before the cds integration block:" >&2
	echo "export PATH=\"$cargo_bin:\$PATH\"" >&2
fi

mkdir -p "$cache_dir"
echo "Restarting cds daemon..."
"$cargo_bin/cds" --restart-daemon

cat <<EOF

cds installed.

Next steps:
  1. Reload your shell:
       source "$profile"

  2. Initialize configured directories if you have not already:
       cds --init

EOF
