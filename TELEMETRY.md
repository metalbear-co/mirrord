# Telemetry / Analytics

mirrord sends anonymous usage statistics to our systems.
We don't store IP addresses, and in the open-source version of mirrord, we don't create any unique identifier for the user. In mirrord for Teams, a random key is used as an identifier for each user.

Data collected is session duration and what features were used (steal/mirror/fs mode, etc).
This helps us to improve the product and by better understanding our users.
Types of data sent:
1. Feature on/off
2. Feature enum value (steal/mirror, read/write)
3. Feature count (how many ports in listen_ports)

## Disabling

Telemetry can be disabled by specifying the following in the mirrord config file:
```json
{"telemetry": false}
```
