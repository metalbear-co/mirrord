import os
import uuid
import unittest

TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."


class FileOpsTest(unittest.TestCase):
    def setUp(self):
        """
        Check if the default file exists.
        """
        with open("/app/test.txt", "r") as f:
            f.seek(0)
            self.assertEqual(f.readline(), TEXT)

    def test_read_write_family(self):
        """
        Reads data from a file in "/tmp" and verifies the text expected is the same as the text written.
        """
        file_path, _ = self._create_new_tmp_file()
        with open(file_path, "r+") as rw_file:
            rw_file.seek(0)
            read_text = rw_file.readline()
            self.assertEqual(read_text, TEXT)

    def test_lseek(self):
        """
        Seeks character by character in a file with Lorem Ipsum text in "/tmp" and verifies the concatenation of the text.
        """
        read_str = ""
        file_path, _ = self._create_new_tmp_file()
        with open(file_path, "r+") as rw_file:
            while read_str != TEXT:
                read_str += rw_file.read(1)
            self.assertEqual(read_str, TEXT)

    def test_openat(self):
        """
        Opens a directory i.e. "/tmp", then opens a file in temp using openat give the directory file desciptor for "/tmp".
        Asserts, data written to the file is the same as being expected.
        """
        file_path, file_name = self._create_new_tmp_file()
        dir = os.open(
            "/tmp", os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC | os.O_DIRECTORY
        )
        file = os.open(file_name, os.O_RDWR | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=dir)
        read = os.read(file, len(TEXT) + 1)
        self.assertEqual(read.decode("utf-8"), TEXT)
        os.close(file)
        os.close(dir)

    def test_mkdir(self):
        """
        Creates a new directory in "/tmp" and verifies if the directory exists.
        """
        os.mkdir("/tmp/test_mkdir")
        self.assertTrue(os.path.isdir("/tmp/test_mkdir"))
    
    def test_mkdirat(self):
        """
        Creates a new directory in "/tmp" using mkdirat given the directory file descriptor for "/tmp" and verifies if the directory exists.
        """
        dir = os.open(
            "/tmp", os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC | os.O_DIRECTORY
        )
        os.mkdir("test_mkdirat", dir_fd=dir)
        self.assertTrue(os.path.isdir("/tmp/test_mkdirat"))
        os.close(dir)

    def _create_new_tmp_file(self):
        """
        Creates a new file in /tmp and returns the path and name of the file.
        """
        file_path = "/tmp/" + (file_name := str(uuid.uuid4()))
        with open(file_path, "w") as w_file:
            w_file.write(TEXT)
        return file_path, file_name


if __name__ == "__main__":
    unittest.main()
