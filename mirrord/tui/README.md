# mirrord-tui

A terminal user interface for [mirrord](https://github.com/metalbear-co/mirrord).

Shipped as the CLI's `mirrord tui` subcommand, and buildable on its own as the `mirrord-tui` binary.

## Working on it

Run from this directory, `cargo` selects only this package, so none of the CLI, layer or agent is
built:

```bash
cargo run     # build and run just the interface
cargo test
cargo clippy
```

With logging, which the standalone binary reads (under `mirrord tui` the CLI owns the logger
instead):

```bash
MIRRORD_LOG_FILE=/tmp/mirrord-tui.log MIRRORD_LOG="warn,mirrord_tui=trace" cargo run
```

`SPEC.md` describes what the interface currently does, and is kept in sync with the code — see
`AGENTS.md`.
