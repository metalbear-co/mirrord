""" Unlink and unlinkat hooks tests """

import os
import unittest
import uuid

TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\n"


class FileOpsTest(unittest.TestCase):
    def test_unlink_remote_readwrite(self):
        """
        Creates a file and removes the link to it using unlink
        """
        test_dir = "/tmp/remote/test_unlink"
        os.makedirs(test_dir, exist_ok=True)
        temp_file = self._create_new_tmp_file(test_dir)[0]
        os.unlink(temp_file)

    def test_unlinkat_remote_readwrite(self):
        """
        Creates a file and removes the link to it using unlinkat
        """
        test_dir = "/tmp/remote/test_unlinkat"
        os.makedirs(test_dir, exist_ok=True)
        temp_file = self._create_new_tmp_file(test_dir)[0]
        os.unlink(temp_file, dir_fd=0)

    def test_unlink_local(self):
        """
        Creates a file and removes the link to it using unlink
        """
        test_dir = "/tmp/local/test_unlink"
        os.makedirs(test_dir, exist_ok=True)
        temp_file = self._create_new_tmp_file(test_dir)[0]
        os.unlink(temp_file)

    def test_unlinkat_local(self):
        """
        Creates a file and removes the link to it using unlink
        """
        test_dir = "/tmp/local/test_unlinkat"
        os.makedirs(test_dir, exist_ok=True)
        temp_file = self._create_new_tmp_file(test_dir)[0]
        os.unlink(temp_file, dir_fd=0)

    def _create_new_tmp_file(self, dir: str):
        """
        Creates a new blank file in @dir and returns the path and name of the file.
        """
        file_path = dir + "/" + (file_name := str(uuid.uuid4()))
        with open(file_path, "w") as w_file:
            w_file.write(TEXT)
        return file_path, file_name


if __name__ == "__main__":
    unittest.main()
