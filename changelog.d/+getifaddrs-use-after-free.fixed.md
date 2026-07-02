Fixed a use-after-free in the macOS `getifaddrs` hook (used when `hide_ipv6_interfaces` is enabled) that could make applications see garbage interface addresses and fail DNS resolution.
