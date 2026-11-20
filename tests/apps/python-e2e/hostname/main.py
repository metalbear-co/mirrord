import socket
import unittest

class Hostname(unittest.TestCase):
	def test_contains_hostname_echo(self):
		self.assertIn('hostname-echo', socket.gethostname())

	def test_trimmed(self):
		hostname = socket.gethostname()
		self.assertEqual(hostname.rstrip(), hostname)

	def test_ips(self):
		hostname = socket.gethostname()
		# Verify that https://github.com/metalbear-co/mirrord/issues/3098 is fixed.
		socket.gethostbyname(hostname)

if __name__ == "__main__":
	unittest.main()
