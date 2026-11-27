"""Prints what this process got of one environment variable, as `<name>=<value>`.

Run as `print_env_var.py <name>`. An unset variable prints as `<name>=None`.
"""

import os
import sys

name = sys.argv[1]
print(f"{name}={os.environ.get(name)}", flush=True)
