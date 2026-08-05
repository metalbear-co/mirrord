The agent accepts QUIC connections from the mirrord Operator on its client port, in addition to
TCP. A stream per logical connection removes the head-of-line blocking that a single TCP connection
multiplexing a whole session is subject to. Operators that do not support QUIC, and clusters where
UDP to the agent is blocked, keep using TCP.
