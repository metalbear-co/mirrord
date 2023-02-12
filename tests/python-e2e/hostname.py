import socket
import unittest

class Hostname(unittest.TestCase):

	def test_is_equal(self):
		self.assertIn('hostname-echo', socket.gethostname())

if __name__ == "__main__":
	unittest.main()
