#!/usr/bin/env bash
# With this shebang what happens when this script is executed normally:
# 1. env is executed.
# 2. env executes bash (calls execve)
# 3. bash executes (calls execve) the binaries that are called in the script. Here: cat.

# With mirrord's SIP bypassing mechanism:
# 1. We create a SIP-free version of env and a version of this script that has the SIP-free env in its shebang.
# 2. We call that version of the script instead of the original.
# 3. The patched version of env is executed and mirrrod's layer is loaded into it.
# 4. env executes bash, mirrrord intercepts that call, makes a SIP-free version of bash and executes that one instead.
# 5. bash executes cat, mirrrord intercepts that call, makes a SIP-free version of cat and executes that one instead.

# cat is a SIPed binary on mac.
cat /very_interesting_file
# sleep so we get close request (else bash might exit before we have time to close it)
sleep 10
