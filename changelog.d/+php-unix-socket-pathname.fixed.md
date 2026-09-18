Fixed Unix stream socket pathname truncation when clients like PHP report `addrlen` without the trailing NUL, allowing the complete path to reach outgoing matching and forwarding.
