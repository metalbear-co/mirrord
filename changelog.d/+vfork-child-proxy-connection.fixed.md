Fixed a child process started through `vfork` not getting its own internal proxy
connection, because the `fork` hook was bypassed when reached from the `vfork`
hook.
