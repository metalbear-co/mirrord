# Debugging mirrord

Debugging mirrord can get hard since we're running from another app flow, so the fact we're debugging might affect the program and make it unusable/buggy (due to sharing stdout with scripts/other applications).

The recommended way to do it is to use `mirrord-console`. It is a small application that receives log information from different mirrord instances and prints it, controlled via `RUST_LOG` environment variable.

To use mirrord console, run it:
`cargo run --bin mirrord-console`

Then run mirrord with the environment variable:
`MIRRORD_CONSOLE_ADDR=127.0.0.1:11233`