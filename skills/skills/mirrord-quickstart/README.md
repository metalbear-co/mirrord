# mirrord-quickstart

Get started with mirrord from zero to first session.

## What it does

This skill helps AI agents guide new users through:
- **Requirements** — verify kubectl and cluster access
- **Installation** — CLI, VS Code, or IntelliJ
- **First session** — connect to a pod and verify it works

## Example prompts

```
"I'm new to mirrord, how do I get started?"

"Help me install mirrord on macOS"

"How do I run my first mirrord session?"

"Connect my local Node.js app to a Kubernetes pod"
```

## Installation options

Use the [official installation guide](https://mirrord.dev/docs/overview/quick-start/) — avoid downloading and executing install scripts via the shell from the network. Options include approved package managers, pinned release binaries (verify checksums), VS Code, and IntelliJ.

## First session

```bash
# List available targets
mirrord ls

# Run your app with mirrord
mirrord exec --target pod/<pod-name> -- <your-command>
```

## Next steps

- `mirrord ui` — local dashboard with a live event stream for active sessions
- `mirrord up` — debug several microservices together (see the **mirrord-up** skill)
- Browser extension — route Chrome requests to your local process

## Learn more

- [mirrord Documentation](https://metalbear.com/mirrord/docs/)
- [GitHub Repository](https://github.com/metalbear-co/mirrord)
