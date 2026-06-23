Fixed a segfault in the macOS DNS configuration hook (`dns_configuration_free`) that crashed short-lived Node workers (e.g. Next.js/Turbopack) on macOS 26 when remote DNS config could not be built.
