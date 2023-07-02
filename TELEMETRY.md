# Telemetry / Analytics

mirrord sends anonymous usage statistics to our systems.
We don't store IP addresses, and we don't create any unique identifier for the user.

Data collected is session duration, what features were used (steal/mirror/fs mode, etc).
This enables us to improve the product and understand the userbase better.
Types of data sent:
1. Feature on/off
2. Feature enum value (steal/mirror, read/write)
3. Feature count (how many ports in listen_ports)

## Disabling

Can be disabled by specifying in the mirrord config file
```json
{"telemetry": false}
```
