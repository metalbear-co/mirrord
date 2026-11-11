"""Prints rules_python's interpreter and the path that holds this target's pip packages.

Bazel's layer and end-to-end tests run this to find the interpreter and the path, and then run
the Python apps with them (see `bazel_python` in `src/lib.rs`).
"""

import os
import sys

print(sys.executable)
print(os.pathsep.join(path for path in sys.path if path))
