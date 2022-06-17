import os
import random
import unittest
#import subprocess
import uuid 

from pyrsistent import s

TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."


class FileOpsTest(unittest.TestCase):
    def test_read_write_family(self):
        """
        O_RDONLY|O_NONBLOCK|O_CLOEXEC|O_DIRECTORY
        """                
        file_path = "/tmp/" + str(uuid.uuid4())
        with open(file_path, "w") as w_file:
            w_file.write(TEXT)
        self.assertFalse(self._check_path_exists_on_host(file_path))
        with open(file_path, "r+") as rw_file:
            rw_file.seek(0)
            read_text = rw_file.readline()            
            self.assertEqual(read_text, TEXT)
            rw_file.close()        
    
    def test_lseek(self):
        read_str = ""        
        file_path = "/tmp/" + str(uuid.uuid4())
        with open(file_path, "w") as w_file:
            w_file.write(TEXT)
        self.assertFalse(self._check_path_exists_on_host(file_path))
        with open(file_path, "r+") as rw_file:
            while read_str != TEXT:
                read_str += rw_file.read(1)
            self.assertEqual(read_str, TEXT)      

    # def test_openat(self):
    #     file_path, file_name = self._create_new_tmp_file()
    #     dir = os.open("/tmp", os.O_RDONLY|os.O_NONBLOCK|os.O_CLOEXEC|os.O_DIRECTORY)
    #     file = os.open(file_name, os.O_RDWR|os.O_NONBLOCK|os.O_CLOEXEC, dir_fd=dir)
    #     self.assertFalse(self._check_path_exists_on_host(file_path))
    #     read = os.read(file, len(TEXT)+1)
    #     self.assertEqual(read, TEXT)


    def _check_path_exists_on_host(self, path):
            # Todo: use the subprocess module here 
            return os.path.exists(path)    

    def _create_new_tmp_file(self):
        file_name = str(uuid.uuid4())
        file_path = "/tmp/" + str(uuid.uuid4())
        file = open(file_path, "w")
        file.write(TEXT)
        file.close()
        return file_path, file_name

if __name__ == "__main__":
    unittest.main()

# dir = os.open("/tmp", os.O_RDONLY|os.O_NONBLOCK|os.O_CLOEXEC|os.O_DIRECTORY)
#         file = os.open("bhjk", os.O_RDWR|os.O_NONBLOCK|os.O_CLOEXEC, dir_fd=dir)
#         print(dir, file)
#         print(os.read(file, 1024))