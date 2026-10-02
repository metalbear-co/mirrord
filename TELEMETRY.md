# Telemetry / Analytics

mirrord sends anonymous usage statistics to our systems.
We don't store IP addresses, and a random key is used as an identifier for each user. In mirrord for Teams, a random key identifying the operator used is sent as well.

Data collected is session duration and what features were used (steal/mirror/fs mode, etc).
This helps us to improve the product and by better understanding our users.
Types of data sent:
1. Feature on/off
2. Feature enum value (steal/mirror, read/write)
3. Feature count (how many ports in `listen_ports`)

When there's an error, we send the name of the error (out of a hard-coded list, so there's no risk of any sensitive data being sent).

## `mirrord up`

`mirrord up` emits one additional anonymous event per invocation summarizing the multi-service configuration: the number of services, which YAML fields are populated and across how many services, which run types (`exec` / `container`) are used, whether a custom `--key` was provided, the success/failure outcome, and (on failure) a coarse error category (`config_validation` / `service_crash` / `internal_error`). No service names, config values, target paths, or command arguments are included.

The opt-out below disables this event as well; setting `common.telemetry: false` in `mirrord-up.yaml` is honored the same way as `telemetry: false` in a regular mirrord config.

## `mirrord tui`

The terminal interface sends reports on the following events:
- TUI start
- a mirrord session was launched from the targets view
- preview environments were stopped - with how many that one command stopped
- TUI exit - with how many times each tab was switched to

Every one of these carries a random identifier for the run, so the events of a single run can be grouped;
it is generated per run and is not stored or reused. No target names, namespaces, cluster identifiers or
config values are included.

The opt-out below disables all of it.

## `mirrord operator install` and `mirrord operator uninstall`

Each run of `mirrord operator install` and `mirrord operator uninstall` sends one anonymous event with the outcome of the run and how long it took. The outcome is success, failure, that the user declined the confirmation prompt, or that the user stopped the command (for example with Ctrl+C). For `mirrord operator uninstall`, the outcome can also be that no operator was installed. When the run fails or is stopped, the event also has the step that it ended in, from a fixed list (for example "fetching the manifest" or "waiting for the operator"). The `mirrord operator install` event also has whether `--api-key` was given and whether a trial was started.

No API key, claim URL, cluster ID, Kubernetes context, namespace name or manifest content is included.

These commands do not read a mirrord config file. To disable their events, use the environment variable below.

## Disabling

Telemetry can be disabled by specifying the following in the mirrord config file:
```json
{"telemetry": false}
```

Alternatively, in the wizard, it is disabled via command-line flag:

```bash
mirrord wizard --telemetry=false
```

For `mirrord operator install` and `mirrord operator uninstall`, it is disabled via environment variable:

```bash
MIRRORD_TELEMETRY=false mirrord operator install
```
