""" Unlink and unlinkat hooks tests """

import os
import unittest


class FileOpsTest(unittest.TestCase):
    def test_unlink_remote_readwrite(self):
        """
        Creates a file and removes the link to it using unlink
        """
        os.mkdir("/tmp/test_unlink")
        os.unlink("/tmp/test_unlink")

    def test_unlinkat_remote_readwrite(self):
        """
        Creates a file and removes the link to it using unlinkat
        """
        os.mkdir("/tmp/test_unlinkat")
        os.unlink("/tmp/test_unlinkat", dir_fd=0)


if __name__ == "__main__":
    unittest.main()
