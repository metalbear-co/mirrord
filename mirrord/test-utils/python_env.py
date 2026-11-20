"""Prints the interpreter and the `PYTHONPATH` that the tests run the Python apps with.

The tests run this program once, and then run the apps with the interpreter directly, because Bazel
starts this program with a shell script that the layer would load into (see `bazel_python`).
"""

import os
import sys

print(sys.executable)
print(os.pathsep.join(path for path in sys.path if path))
