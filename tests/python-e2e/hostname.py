import socket
import unittest

class Hostname(unittest.TestCase):

	def test_is_equal(self):
		self.assertEq(socket.gethostname(), 'hostname-echo')

if __name__ == "__main__":
	unittest.main()
