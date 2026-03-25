Use a tokio socket in the `outgoing_disabled_udp` task to avoid blocking the (now single) tokio thread.
