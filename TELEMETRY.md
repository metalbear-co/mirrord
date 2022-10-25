# Telemetry

mirrord sends anonymous usage statistics to our systems. The information sent is:
1. mirrord version.
2. cli/extension.
3. platform (linux, macos).

In our databases, we don't store IP, and we don't create any unique identifier for the user.

## Disabling

### CLI
You can disable telemetry by specifying `--no-telemetry`.

### Extension
In the settings of the extension.