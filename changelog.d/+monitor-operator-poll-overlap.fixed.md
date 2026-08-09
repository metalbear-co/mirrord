Stopped the session monitor stacking cluster-session polls on top of each other, and gave each one a timeout so a slow cluster is reported as a timeout instead of an unreachable server.
