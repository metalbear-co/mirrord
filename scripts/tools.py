"""The code that `lint.py` and `format.py` share: they run the tools that they take from `PATH` the
same way, and both run scripts of the frontends, which pnpm installs.
"""

import shutil
import subprocess
import sys

FRONTENDS = ["mirrord-ui", "mirrord-wizard", "session-monitor-frontend"]

# Installs the frontends' dependencies, like ESLint and Prettier, which their scripts run.
PNPM_INSTALL = ["pnpm", "install", "--frozen-lockfile", "--silent"]


def run(command, output=None):
    """Runs a command, and returns whether it passed. A command whose program isn't on `PATH`
    fails. The command prints to the file `output`, or to this program's own output without it."""
    # `shutil.which` also finds a program by the extensions in `PATHEXT` on Windows, such as
    # `pnpm.cmd`, which `subprocess.run` doesn't look for.
    program, *arguments = command
    path = shutil.which(program)
    if path is None:
        print(f"`{program}` isn't on PATH.", file=output or sys.stdout, flush=True)
        return False
    process = subprocess.run(
        [path, *arguments], stdout=output, stderr=subprocess.STDOUT if output else None, check=False
    )
    return process.returncode == 0
