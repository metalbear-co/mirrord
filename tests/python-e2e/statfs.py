import os
import unittest

class StatFsTest(unittest.TestCase):
    def check_stat_success(self):
        os.statvfs("/tmp/test_file.txt")


if __name__ == "__main__":
    unittest.main()