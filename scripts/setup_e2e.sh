#!/bin/bash

sudo apt-get update && sudo apt-get install -y curl

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sudo sh -s -- -y

curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.4/install.sh | bash
export NVM_DIR="$HOME/.nvm"
[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"  # This loads nvm
[ -s "$NVM_DIR/bash_completion" ] && \. "$NVM_DIR/bash_completion"  

nvm install 14
nvm use 14

npm install express

pip3 install flask
pip3 install fastapi
pip3 install uvicorn[standard]

sudo apt-get install bison
bash < <(curl -s -S -L https://raw.githubusercontent.com/moovweb/gvm/master/binscripts/gvm-installer)

source /home/runner/.gvm/scripts/gvm

gvm install go1.18
gvm use go1.18

go version

bash ./scripts/build_go_apps.sh 18

gvm install go1.19
gvm use go1.19

go version

bash ./scripts/build_go_apps.sh 19

gvm install go1.20
gvm use go1.20

go version

bash ./scripts/build_go_apps.sh 20

cargo build --manifest-path=./tests/rust-e2e-fileops/Cargo.toml
cargo build --manifest-path=./tests/rust-unix-socket-client/Cargo.toml
cargo build --manifest-path=./tests/rust-bypassed-unix-socket/Cargo.toml
