import socket
import unittest

class Hostname(unittest.TestCase):
	def test_contains_hostname_echo(self):
		self.assertIn('hostname-echo', socket.gethostname())

	def test_trimmed(self):
		hostname = socket.gethostname()
		self.assertEqual(hostname.rstrip(), hostname)

if __name__ == "__main__":
	print("{} socket".format(socket.gethostname()))
	unittest.main()
