# cds

`cds` is a `cd` replacement with semantic directory search.

`cds` has full `cd` compatibility via a passthrough. You can use `cds` exactly like
you use `cd` today. 

## What does it do? 
Have you ever wanted to go to a directory but you forget *exactly* what it is and where it is? 
cds solves this problem. 

`cds the chrome extension I worked on last month` will take you right there. 

## How does it work? 

When you first download cds, you define directories that you'd like to index. `~/Projects` for example.

When you then run `cds --init`, cds will scan all of the folders/files in those directories (excluding hidden directories
and sensitive files) and generate embeddings for them that are stored in a local SQLite database. 

When you later run `cds the chrome extension I worked on last month` cds will search that embeddings data to find
the directory you're looking for and invoke cd to navigate you there. 

#### Why not my entire system? 
Embeddings get expensive fast and I have to use a very tiny model (bge-small-en-v1.5) to keep this lean.
If we have a ton of parameteres in the database A: The db would be *huge* and B: The search would take a lot of time.

## Build

```sh
cargo build
```

## Install

From the repository root:

```sh
./install.sh
```

The installer runs `cargo install --path .` and adds an idempotent shell integration
block to your `~/.zshrc` or `~/.bashrc`. After it finishes:

```sh
source ~/.zshrc
cds --init
```

Use `source ~/.bashrc` instead if you use bash.

To replace an existing installed binary:

```sh
./install.sh --force
```

## Test

```sh
cargo test --all
```

The test suite includes a Docker-backed equivalence test. When Docker is available, it
mounts this project into a Rust container, builds `cds`, generates a fresh random
filesystem tree, and compares `cds` against the shell's built-in `cd` for exit status,
stdout, stderr, `PWD`, `OLDPWD`, and physical path behavior.

Use a specific container image with:

```sh
CDS_DOCKER_IMAGE=rust:1 cargo test --test docker_cd_equivalence
```

The Docker image must include Cargo and the C++ standard library/linker support required by
the embedding runtime. The default image is `rust:1`.

## Indexing

`cds` only indexes directories you configure. It does not crawl the whole filesystem.

Create the default config and SQLite database, then index the configured roots:

```sh
cds --init
```

By default this creates:

```text
~/.config/cds/config.json
~/.local/share/cds/cds.sqlite
```

The initial config looks like this:

```json
{
  "index": {
    "roots": ["~/Projects"],
    "exclude": [
      ".git",
      "node_modules",
      "target",
      "dist",
      "build",
      ".next",
      ".cache",
      ".venv",
      "venv",
      "vendor",
      "*.xcassets",
      "*.imageset",
      "*.appiconset",
      "*.colorset"
    ],
    "max_file_bytes": 65536,
    "max_excerpt_bytes": 4096,
    "max_entries_per_directory": 80,
    "max_depth_per_top_level_directory": 3,
    "max_chunk_bytes": 4096
  },
  "detectors": []
}
```

Run an indexing pass over configured roots:

```sh
cds --index
```

Or index explicit roots:

```sh
cds --index ~/Projects ~/work
```

The current indexer stores directory metadata and text-file content separately in SQLite.
Directory rows store structured filesystem metadata: name, type, parent path, size in bytes,
created time, modified time, accessed time, readonly status, and index time. Text files are
stored as file metadata plus embedded content chunks linked back to their containing
directories. Directory metadata itself is not embedded.
Hidden directories are always skipped and pruned from the local index. Each configured index
root is treated as a container, and each top-level directory inside it is indexed only through
`max_depth_per_top_level_directory` levels.

Text-file chunks are embedded locally with `BAAI/bge-small-en-v1.5` through FastEmbed/ONNX.
Model files are cached under `~/.cache/cds/models` by default, or under `CDS_CACHE_DIR/models`
when that environment variable is set. Tests can force deterministic fake embeddings with
`CDS_EMBEDDER=fake`.

## Directory Types

Directory type inference is rule-based and data-driven. `cds` does not ask the embedding
model to guess whether a directory is a `rust project`, `chrome extension`, `rails app`, etc.
Instead, it runs JSON detectors against each indexed directory during `cds --init` and
`cds --index`.

Built-in detectors live in `src/index/directory_types/` and currently cover:

- `chrome extension`
- `database migrations`
- `next.js app`
- `node project`
- `python project`
- `rails app`
- `rust project`

Each detector has a `label` and one or more `rules`. A rule matches only when all of its
`signals` match. If a detector has multiple matching rules for the same directory, `cds`
keeps one classification for that label and prefers the highest-confidence rule. Each stored
classification includes the directory path, label, confidence, rule id, evidence path,
evidence summary, and detection timestamp.

Custom detectors are added to the `detectors` array in `~/.config/cds/config.json`.
`cds --index` loads that array every time it indexes, so editing the config and re-running
`cds --index` is enough to apply a new directory type locally.

Example detector:

```json
{
  "label": "rust project",
  "rules": [
    {
      "id": "cargo_toml_package",
      "confidence": 0.98,
      "evidence_summary": "Cargo.toml contains [package] or [workspace]",
      "signals": [
        {
          "kind": "file_contains",
          "path": "Cargo.toml",
          "contains_any": ["[package]", "[workspace]"]
        }
      ]
    }
  ]
}
```

Supported signal kinds:

- `file_exists`: matches when a relative file path exists inside the directory.
- `file_contains`: reads a small text file and checks for text. Use `contains_any`,
  `contains_all`, or both.
- `directory_name`: checks the directory's own name with `equals_any` and/or
  `contains_any`.
- `child_name`: checks immediate child names with `contains_any`, `starts_with_any`,
  and/or `ends_with_any`.

Signals are case-insensitive. Paths are relative to the directory being classified.
`file_contains` ignores binary files and files larger than 128 KiB.

For example, a custom detector for Terraform projects can be added directly to
`~/.config/cds/config.json`:

```json
{
  "index": {
    "roots": ["~/Projects"],
    "exclude": [
      ".git",
      "node_modules",
      "target",
      "dist",
      "build",
      ".next",
      ".cache",
      ".venv",
      "venv",
      "vendor",
      "*.xcassets",
      "*.imageset",
      "*.appiconset",
      "*.colorset"
    ],
    "max_file_bytes": 65536,
    "max_excerpt_bytes": 4096,
    "max_entries_per_directory": 80,
    "max_depth_per_top_level_directory": 3,
    "max_chunk_bytes": 4096
  },
  "detectors": [
    {
      "label": "terraform project",
      "rules": [
        {
          "id": "terraform_files",
          "confidence": 0.95,
          "evidence_summary": "directory contains Terraform files",
          "signals": [
            {
              "kind": "child_name",
              "ends_with_any": [".tf", ".tfvars"]
            }
          ]
        }
      ]
    }
  ]
}
```

After saving the config:

```sh
cds --index
cds --dir-type-count
```

To add a built-in/community detector to the project itself, add a new JSON file under
`src/index/directory_types/`, then register it in `BUILTIN_DEFINITIONS` in
`src/index/classify.rs`. Prefer high-confidence signals that are difficult to trigger by
accident. For example, checking that `manifest.json` contains `"manifest_version"` and at
least one browser-extension field is better than classifying every directory with any
`manifest.json` as a Chrome extension.

Search indexed directories:

```sh
cds --search chrome extension
```

List detected directory types:

```sh
cds --dir-type-count
```

Delete all indexed data from the SQLite database:

```sh
cds --reset
```

This prompts before deleting directory metadata, file metadata, content chunks, and directory
type classifications. The database file and schema are kept in place.

When the shell integration is installed, `cds` also tries semantic search automatically for
plain directory changes that do not look like local `cd` usage. For example, `cds Projects`
first checks whether `./Projects` exists exactly. If it does, `cds` delegates to the shell's
built-in `cd` exactly as usual. If it does not, `cds` searches the index and emits a `cd` to
the best existing indexed directory. Flags, `-`, `--`, `~`, `.`, `..`, and paths containing
`/` always use normal `cd` behavior.

## Shell Setup

Install the shell integration in your current shell:

```sh
eval "$(command target/debug/cds --shell-init zsh)"
```

For bash:

```sh
eval "$(command target/debug/cds --shell-init bash)"
```

After that, use `cds` like `cd`:

```sh
cds
cds -
cds -P ../somewhere
cds -- -directory-starting-with-dash
```

To make this permanent after `cds` is installed somewhere on `PATH`, add the appropriate
line to your shell profile:

```sh
eval "$(command cds --shell-init zsh)"
```
