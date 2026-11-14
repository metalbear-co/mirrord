Fixed mirrord panicking on macOS when it ran a script without a shebang while the `SHELL` environment variable wasn't set. It runs such a script with `/bin/sh` now, as a shell would.
