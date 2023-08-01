Sometimes the internal proxy doesn't flush before we do redirection then caller can't read port
leading to "Couldn't get port of internal proxy"