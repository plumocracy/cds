#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ -n "${CDS_BIN:-}" ]; then
	cds_bin="$CDS_BIN"
elif command -v cds >/dev/null 2>&1; then
	cds_bin="$(command -v cds)"
elif [ -x "$repo_dir/target/debug/cds" ]; then
	cds_bin="$repo_dir/target/debug/cds"
elif [ -x "$repo_dir/target/release/cds" ]; then
	cds_bin="$repo_dir/target/release/cds"
else
	echo "error: could not find cds binary" >&2
	echo "Run 'cargo build' or './install.sh' first, or set CDS_BIN=/path/to/cds." >&2
	exit 1
fi

exec "$cds_bin" --restart-daemon
