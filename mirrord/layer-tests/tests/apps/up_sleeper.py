"""A `mirrord up` service that writes its PID to a file, then sleeps until it is stopped.

Usage: up_sleeper.py <partial> <pid_file>

The PID is written to `partial`, then renamed to `pid_file`, so a reader never sees half of it.
"""

import os
import sys
import time

partial, pid_file = sys.argv[1:]
with open(partial, "w") as file:
    file.write(str(os.getpid()))
os.replace(partial, pid_file)
time.sleep(60)
