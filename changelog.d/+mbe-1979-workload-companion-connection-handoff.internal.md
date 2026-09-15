Add mirrord-agent support for accepting connections handed off from a companion
process running alongside the workload, via a new shared handoff protocol crate
and a `workload_companion` module that decides whether to accept or decline
handed-off connections based on currently subscribed ports. This is the
agent-side counterpart to the socket interception added in
metalbear-co/operator#2388.
