""" Files ready only feature test """
import os
import uuid
import unittest

TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."


class FileOpsTest(unittest.TestCase):
    def test_read_only(self):
        """
        Overwrite a file that exists in the container, then read it verifying we didn't actually overwrite it.
        """
        with open("/app/test.txt", "wb") as f:
            f.write(b"nothing")
        with open("/app/test.txt", "r") as f:
            f.seek(0)
            self.assertEqual(f.readline(), TEXT)


if __name__ == "__main__":
    unittest.main()