""" Unlink and unlinkat hooks tests """

import os
import unittest
import uuid

TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum.\n"


class FileOpsTest(unittest.TestCase):
    def test_unlink_remote_readwrite(self):
        """
        Creates a file remotely and removes the link to it using unlink
        """
        # create test dir and temp file
        test_dir = "/tmp/remote_test/test_unlink"
        os.makedirs(test_dir, exist_ok=True)
        temp_file = self._create_new_tmp_file(test_dir)[0]

        # check file exists and open it, then call unlink
        # if file is not opened, it will be deleted by unlink, but we want to just unlink it
        self.assertTrue(os.path.isfile(temp_file))
        file = open(temp_file, "r")
        os.unlink(temp_file)

        # release our hold on file and check it has been removed
        file.close()
        self.assertFalse(os.path.isfile(temp_file))

        # def test_unlinkat_remote_readwrite(self):
        """
        Creates a file remotely and removes the link to it using unlinkat
        """
        # create test dir
        test_dir = "/tmp/remote_test/test_unlinkat"
        os.makedirs(test_dir, exist_ok=True)
        self.assertTrue(os.path.isdir(test_dir))

        # create test file
        (test_file_abs_path, test_file_name) = self._create_new_tmp_file(test_dir)

        # call unlink with dir_fd, which will call unlinkat under the hood
        # get file descriptor for the parent directory (must be local)
        dir_fd = os.open(test_dir, os.O_RDONLY | os.O_DIRECTORY)
        os.unlink(test_file_name, dir_fd=dir_fd)

        # manually close the directory after unlink
        os.close(dir_fd)
        self.assertFalse(os.path.isfile(test_file_abs_path))

    def test_unlink_local(self):
        """
        Creates a file locally and removes the link to it using unlink
        """
        # create test dir and temp file
        test_dir = "/tmp/local_test/test_unlink"
        os.makedirs(test_dir, exist_ok=True)
        temp_file = self._create_new_tmp_file(test_dir)[0]

        # check file exists and open it, then call unlink
        # if file is not opened, it will be deleted by unlink, but we want to just unlink it
        self.assertTrue(os.path.isfile(temp_file))
        file = open(temp_file, "r")
        os.unlink(temp_file)

        # release our hold on file and check it has been removed
        file.close()
        self.assertFalse(os.path.isfile(temp_file))

    def test_unlinkat_local(self):
        """
        Creates a file locally and removes the link to it using unlinkat
        """
        # create test dir
        test_dir = "/tmp/local_test/test_unlinkat"
        os.makedirs(test_dir, exist_ok=True)
        self.assertTrue(os.path.isdir(test_dir))

        # create test file
        (test_file_abs_path, test_file_name) = self._create_new_tmp_file(test_dir)

        # call unlink with dir_fd, which will call unlinkat under the hood
        # get file descriptor for the parent directory (must be local)
        dir_fd = os.open(test_dir, os.O_RDONLY | os.O_DIRECTORY)
        os.unlink(test_file_name, dir_fd=dir_fd)

        # manually close the directory after unlink
        os.close(dir_fd)
        self.assertFalse(os.path.isfile(test_file_abs_path))

    def test_unlink_mapped(self):
        """
        Creates a file on a mapped path and removes the link to it using unlink
        """
        # create test dir and temp file, ensure path containing source_test
        # is mapped to path containing sink_test
        test_dir = "/tmp/source_test/test_unlink"
        os.makedirs(test_dir, exist_ok=True)
        self.assertTrue(os.path.isdir("/tmp/sink_test/test_unlink"))
        file_path = self._create_new_tmp_file(test_dir)[0]

        # check file exists and open it, then call unlink
        # if file is not opened, it will be deleted by unlink, but we want to just unlink it
        self.assertTrue(os.path.isfile(file_path))
        file = open(file_path, "r")
        os.unlink(file_path)

        # release our hold on file and check it has been removed
        file.close()
        self.assertFalse(os.path.isfile(file_path))

    def test_unlinkat_mapped(self):
        """
        Creates a file on a mapped path and removes the link to it using unlinkat
        """
        # create test dir
        test_dir = "/tmp/source_test/test_unlinkat"
        os.makedirs(test_dir, exist_ok=True)
        self.assertTrue(os.path.isdir("/tmp/sink_test/test_unlinkat"))

        # create test file
        (test_file_abs_path, test_file_name) = self._create_new_tmp_file(test_dir)

        # call unlink with dir_fd, which will call unlinkat under the hood
        # get file descriptor for the parent directory
        dir_fd = os.open(test_dir, os.O_RDONLY | os.O_DIRECTORY)
        os.unlink(test_file_name, dir_fd=dir_fd)

        # manually close the directory after unlink
        os.close(dir_fd)
        self.assertFalse(os.path.isfile(test_file_abs_path))

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
