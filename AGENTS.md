# AGENTS.md

## Project Goal

Build `cds`, a Rust command-line tool that acts as a semantic alternative to `cd`.

The user should be able to type a natural-language command such as:

```sh
cds the last chrome extension I worked on
```

and have the tool find the most likely matching directory, then navigate the shell there.

Because a child process cannot directly change the parent shell's working directory, `cds`
must provide a shell integration that evaluates or sources the destination path in the
user's current shell.

## Core Product Requirements

- Store indexed filesystem knowledge in SQLite.
- Generate and search vector embeddings with `bge-small-en-v1.5`.
- Support semantic search over file and directory metadata.
- Prefer directories that are likely useful navigation targets, not arbitrary files.
- Include recency signals so phrases like "last thing I worked on" are meaningful.
- Avoid indexing secrets, generated dependency folders, build artifacts, and large binary
  content by default.
- Be fast enough for interactive shell usage.
- Be deterministic where possible: ranking should be explainable and testable.

## Expected Rust Stack

Prefer stable, well-maintained crates:

- CLI parsing: `clap`
- SQLite: `sqlx` with SQLite
- Serialization: `serde`, `serde_json`
- Error handling: `color_eyre` for binaries, `thiserror` for reusable library errors
- Filesystem walking: `ignore` or `walkdir`; prefer `ignore` because it respects common
  ignore files
- Time handling: `time` or `chrono`
- Logging/tracing: `tracing`, `tracing-subscriber`
- Tests: standard Rust tests plus focused integration tests under `tests/`

For embeddings, prefer an implementation path that keeps the tool locally usable:

- Use `BAAI/bge-small-en-v1.5` through FastEmbed/ONNX for production text embeddings.
- Do not introduce a hosted embedding API as the default path.
- If model download/setup is needed, make it explicit and cache model files outside the
  repo in a user cache directory.
- Keep deterministic fake embeddings available for normal tests with `CDS_EMBEDDER=fake`.

## Suggested Architecture

Keep the code split between a thin binary and testable library modules.
The CLI and application pipeline are async end to end; use `tokio` for the binary runtime
and keep database access on SQLx async APIs.

Suggested modules:

- `cli`: command definitions and argument parsing
- `config`: config file loading, defaults, ignored paths, model/database locations
- `db`: SQLite schema, migrations, queries, and transactions
- `embed`: model loading, text normalization, embedding generation
- `index`: filesystem scanning, file summarization, change detection, batch indexing
- `search`: vector similarity, ranking, recency/path heuristics, result explanations
- `shell`: shell integration output for changing directories

Keep business logic out of `main.rs`.

## SQLite Guidance

SQLite should be the durable source of indexed data.
Schema changes belong in top-level SQLx migration files under `migrations/`.

Track at least:

- indexed paths
- path type: file or directory
- parent directory
- normalized searchable text
- embedding vector
- content hash or metadata fingerprint
- modified time
- indexed time
- lightweight ranking signals, such as recent access or matched child files

Use migrations from the beginning. Keep schema changes explicit and reviewed.

Embedding storage options may include:

- `BLOB` containing packed `f32` values
- `sqlite-vec` or another SQLite vector extension if it is easy to install and test

If using a SQLite extension, preserve a fallback or clear setup error so the CLI does not
fail opaquely.

## Current Command Shape

All explicit `cds` commands should be long flags so they cannot be confused with normal
`cd` operands. Do not add bare subcommands such as `cds init`; those should remain valid
directory names when routed through the shell integration.

Current user-facing commands:

```sh
cds --init
cds --index [PATH...]
cds --search QUERY...
cds --dir-type-count
cds --reset
cds --shell-init [bash|zsh]
```

The shell integration makes the common path concise. Plain invocations such as
`cds Projects` should preserve `cd` behavior unless they are clearly semantic searches.
The hidden shell-machine mode currently uses `--cds-emit`.

The Rust binary should not print human-oriented decoration in machine-readable shell
integration mode. Keep stdout parseable and put diagnostics on stderr.

## Ranking Expectations

Search should combine:

- vector similarity from `bge-small-en-v1.5`
- directory-level aggregation from matching child files
- path/name lexical matches
- recency from filesystem metadata and index history
- penalties for ignored, hidden, generated, or low-signal paths

Ranking changes should be covered by tests with small synthetic fixtures.

## Indexing Rules

Default excludes should include common high-noise directories:

- `.git`
- `node_modules`
- `target`
- `dist`
- `build`
- `.next`
- `.cache`
- virtual environments
- dependency/vendor directories

Do not index obvious secrets by default, including `.env`, private keys, credential files,
or files matching common secret naming patterns.

For file content, prefer small, meaningful text excerpts. Do not embed very large files
whole. Skip binary files unless there is a deliberate metadata-only strategy.

## Development Commands

Once the Rust project is initialized, keep these commands working:

```sh
cargo fmt
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo build
```

Run formatting and tests before handing off changes when practical.

## Testing Guidance

Add tests for:

- SQLite migrations and query behavior
- embedding vector serialization/deserialization
- ignored path filtering
- indexing change detection
- ranking behavior with deterministic fake embeddings
- shell integration output
- Docker-backed `cd` equivalence for Bash behavior

Do not make tests depend on downloading or running the real embedding model unless they
are explicitly marked as slow/integration tests. Use a fake embedder for normal tests.

The Docker equivalence test runs the project inside a Rust container and compares `cds`
against Bash's built-in `cd` for status, stdout, stderr, `PWD`, `OLDPWD`, and physical path.
Keep these details in mind when editing it:

- CI currently uses `CDS_DOCKER_IMAGE=rust:1`.
- The Docker image must include Cargo. The test builds `cds` with `--no-default-features`
  and `CDS_EMBEDDER=fake`, so it does not need the FastEmbed/ONNX dependency stack.
- The test runner must explicitly add `/usr/local/cargo/bin` to `PATH`; some container
  invocations otherwise fail with `cargo: command not found`.
- Bash includes source line numbers in `cd` diagnostics. Normalize only those line numbers
  before comparing stderr, while keeping the actual diagnostic text exact.
- The Docker test can skip locally when Docker is unavailable. Use
  `CDS_DOCKER_IMAGE=rust:1 cargo test --test docker_cd_equivalence -- --nocapture`
  with Docker access when changing equivalence behavior. Set `CDS_DOCKER_RANDOM_CASES`
  to increase or decrease the randomized sample count.

## Repository Hygiene

- Keep generated databases, model files, caches, and local indexes out of git.
- Keep sample fixtures small.
- Prefer explicit config and cache paths under the user's platform cache/config
  directories.
- Avoid committing machine-specific absolute paths.
- Document new commands and shell setup as they are added.

## Agent Workflow

Before editing:

1. Inspect the current tree and existing Rust conventions.
2. Check `git status` and preserve unrelated user changes.
3. Prefer small, reviewable changes that move the CLI toward a working vertical slice.

When implementing:

1. Start with schema, config, and testable library behavior.
2. Use deterministic fake embeddings in tests.
3. Keep shell-facing output stable and minimal.
4. Add or update docs when command behavior changes.

Before finishing:

1. Run the relevant formatting and test commands.
2. Report what changed, which commands passed, and any commands that could not be run.
