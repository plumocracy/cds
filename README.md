# cds

`cds` is a `cd` replacement with semantic directory search.

`cds` has full `cd` compatibility via a passthrough. You can use `cds` exactly like
you use `cd` today. 

## What does it do? 
Have you ever wanted to go to a directory but you forget *exactly* what it is and where it is? 
cds solves this problem. 

`cds the chrome extension I worked on last month` will take you right there. 

This all runs locally via an sqlite backed vector database and a small, local embeddings model.

## Build

```sh
cargo build
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
CDS_DOCKER_IMAGE=rust:1-slim cargo test --test docker_cd_equivalence
```

## Shell Setup

Install the shell integration in your current shell:

```sh
eval "$(target/debug/cds --shell-init zsh)"
```

For bash:

```sh
eval "$(target/debug/cds --shell-init bash)"
```

After that, use `cds` like `cd`:

```sh
cds
cds -
cds -P ../somewhere
cds -- -directory-starting-with-dash
```

To make this permanent, add the appropriate `eval` line to your shell profile after
`cds` is installed somewhere on `PATH`.
