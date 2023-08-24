#!/bin/bash
gvm use go1.18

go version

bash ./scripts/build_go_apps.sh 18

gvm use go1.19

go version

bash ./scripts/build_go_apps.sh 19

gvm use go1.20

go version

bash ./scripts/build_go_apps.sh 20

cargo build --manifest-path=./tests/rust-e2e-fileops/Cargo.toml
cargo build --manifest-path=./tests/rust-unix-socket-client/Cargo.toml
cargo build --manifest-path=./tests/rust-bypassed-unix-socket/Cargo.toml
