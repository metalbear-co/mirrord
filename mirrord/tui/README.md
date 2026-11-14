# mirrord-tui

A terminal user interface for [mirrord](https://github.com/metalbear-co/mirrord).

Shipped as the CLI's `mirrord tui` subcommand, and buildable on its own as the `mirrord-tui` binary.

## Working on it

Bazel builds only what a target depends on, so none of the CLI, layer or agent is built:

```bash
bazel run //mirrord/tui:mirrord-tui   # build and run just the interface
bazel test //mirrord/tui/...
bazel build --config=clippy //mirrord/tui/...
```

With logging, which the standalone binary reads (under `mirrord tui` the CLI owns the logger
instead):

```bash
MIRRORD_LOG_FILE=/tmp/mirrord-tui.log MIRRORD_LOG="warn,mirrord_tui=trace" bazel run //mirrord/tui:mirrord-tui
```

`SPEC.md` describes what the interface currently does, and is kept in sync with the code — see
`AGENTS.md`.
