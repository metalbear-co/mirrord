`cargo xtask test-ut` now runs the CLI unit tests through `cargo nextest` when it
is installed, falling back to `cargo test` otherwise, so those tests get the same
flake retries as every other suite.
