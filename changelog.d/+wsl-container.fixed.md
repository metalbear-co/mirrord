When running `mirrord container` command from a WSL instance, docker bridge between WSL instances
may not be ready by the time mirrord's proxy container sidecar connects its proxy process, e.g.
Docker Desktop WSL to another WSL. Added connection retry mechanism that will make the end-to-end
flow reliable.
