Branch databases and preview sessions report a `Ready` condition, so `kubectl wait --for=condition=Ready` works instead of polling the phase.
