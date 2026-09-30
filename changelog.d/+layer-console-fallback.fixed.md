An unreachable `mirrord-console` no longer aborts the target: the layer reports it
on stderr and logs to its usual file and stderr sinks instead. On Windows, console
logging works again, and a console that stops responding is reported once
instead of stalling the process.
