import unittest

TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."


class FileOpsTest(unittest.TestCase):
    def test_read_write_family(self):
        with open("/tmp/test", "w") as w_file:
            w_file.write(TEXT)
        with open("/tmp/test", "r+") as rw_file:
            rw_file.seek(0)
            read_text = rw_file.readline()
            self.assertEqual(read_text, TEXT)
            rw_file.close()


if __name__ == "__main__":
    unittest.main()
