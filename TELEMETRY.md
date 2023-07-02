# Telemetry

mirrord sends anonymous usage statistics to our systems.
We don't store IP addresses, and we don't create any unique identifier for the user.

Currently, there are version check telemetries, controlled from CLI and IDE and analytics, controlled via config file or cli.

## Version Checks

mirrord checks for version update for cli/IDE.

The information sent is:
1. mirrord version.
2. cli/extension - if IntelliJ - product type (GoLand, PyCharm, etc.).
3. platform (linux, macos).

### Disabling

#### CLI

You can disable version checks by --disable-version-check.

#### IntelliJ

The check is opt-in on first use and can also be controlled from settings.

#### VSCode

We use the [VSCode API](https://code.visualstudio.com/docs/getstarted/telemetry) to check if we can send telemetries (as recommended by them).


## Analytics

This feature sends session duration, what features were used (steal/mirror/fs mode, etc).
This enables us to improve the product and understand the userbase better.
Types of data sent:
1. Feature on/off
2. Feature enum value (steal/mirror, read/write)
3. Feature count (how many ports in listen_ports)


### Disabling 

Can be disabled by specifying in the mirrord config file
```json
{"telemetry": false}
```
