# mirrord-ci

Set up mirrord in CI pipelines for testing against real Kubernetes environments.

## What it does

This skill helps AI agents:
- **Configure** CI runners to connect to Kubernetes clusters
- **Generate** CI workflow files (GitHub Actions, GitLab CI, CircleCI, Jenkins)
- **Set up** `mirrord ci start/stop` commands properly
- **Troubleshoot** CI-specific mirrord issues

## Example prompts

```
"Set up mirrord for GitHub Actions"

"How do I run integration tests against staging in CI?"

"Generate a GitLab CI config that uses mirrord"

"mirrord ci start is not working in my pipeline"
```

## How it works

1. Asks about your CI platform and target environment
2. Verifies prerequisites (mirrord version, kubectl access)
3. Generates platform-specific CI configuration
4. Includes proper setup and cleanup steps
5. Provides troubleshooting guidance

## Key commands

```bash
# Start app with mirrord in background
mirrord ci start --target deployment/my-app -- npm start

# Run your tests
npm test

# Stop all mirrord sessions
mirrord ci stop
```

## Supported CI platforms

- GitHub Actions
- GitLab CI
- CircleCI
- Jenkins
- Any CI platform with shell access

## References

This skill uses local reference files:
- `references/schema.json` - mirrord JSON Schema for config validation
- `references/troubleshooting.md` - Common issues and solutions

## Learn more

- [mirrord for CI Overview](https://metalbear.com/mirrord-for-ci/)
- [mirrord CI Documentation](https://mirrord.dev/docs/using-mirrord/mirrord-for-ci/)
