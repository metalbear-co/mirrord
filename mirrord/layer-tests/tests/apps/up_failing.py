"""A `mirrord up` service that crashes with exit code 3 once the release file exists.

Usage: up_failing.py <release>
"""

import os
import sys
import time

release = sys.argv[1]
deadline = time.time() + 60
while not os.path.exists(release) and time.time() < deadline:
    time.sleep(0.02)
sys.exit(3)
