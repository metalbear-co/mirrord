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

The terminal interface reports each of these as it happens, rather than one summary when it exits: that it
started, that a connection to the cluster failed, that a mirrord session was launched from the targets view,
and that preview environments were stopped (with how many that one command stopped). When the interface is
quit rather than killed, it also reports how many times each tab was switched to.

Every one of these carries a random identifier for the run, so the events of a single run can be grouped;
it is generated per run and is not stored or reused. No target names, namespaces, cluster identifiers or
config values are included. The standalone `mirrord-tui` development binary reports nothing at all.

The opt-out below disables all of it.

## Disabling

Telemetry can be disabled by specifying the following in the mirrord config file:
```json
{"telemetry": false}
```

Alternatively, in the wizard, it is disabled via command-line flag:

```bash
mirrord wizard --telemetry=false
```