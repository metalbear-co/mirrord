Intproxy now no longer runs as a child of the user process so it doesn't get reaped by wait(2) calls. ([#3536](https://github.com/metalbear-co/mirrord/issues/3563))
