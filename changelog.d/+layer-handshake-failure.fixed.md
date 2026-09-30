Fixed the internal proxy ending the whole session when accepting a new layer
connection failed transiently, for example when a process exited while its
connection was still waiting to be accepted.
