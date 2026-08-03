cd .devcontainer; docker build -t mirrord-zed -f Dockerfile.zed

docker run -d \
  --name mirrord-zed \
  --add-host=host.docker.internal:host-gateway \
  --privileged \
  -p 2222:22 \
  -e LOCAL_UID=501 \
  -e LOCAL_GID=20 \
  -v /Users/daniel/git/mirrord:/workspaces/mirrord:rw \
  -v mirrord-cargo-registry:/home/vscode/.cargo/registry \
  -v mirrord-cargo-git:/home/vscode/.cargo/git \
  -v mirrord-target:/workspaces/mirrord/target \
  mirrord-zed

docker cp ~/.ssh/mirrord_zed.pub mirrord-zed:/tmp/authorized_keys
docker exec mirrord-zed bash -lc 'mkdir -p /home/vscode/.ssh && cat /tmp/authorized_keys > /home/vscode/.ssh/authorized_keys && chown -R vscode:vscode /home/vscode/.ssh && chmod 700 /home/vscode/.ssh && chmod 600 /home/vscode/.ssh/authorized_keys'
docker exec mirrord-zed bash -lc 'chown -R vscode:vscode /usr/local/cargo /home/vscode/.cargo/registry /home/vscode/.cargo/git /workspaces/mirrord'
docker exec mirrord-zed bash -lc 'chmod -R a+rw /workspaces/mirrord'

Inside:
cargo xtask build-remote-bootstrap --platform linux-aarch64
cargo build -p capabilities-rust-backend --target aarch64-unknown-linux-gnu --manifest-path sample/capabilities-rust/Cargo.toml

# export OPENSSL_INCLUDE_DIR=/usr/include
# export OPENSSL_LIB_DIR=/usr/lib
