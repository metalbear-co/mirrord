Use nodeSelector when possible for agent creation. Improve capacity/scheduling issues.

When using nodeName directly (old, fallback way) we bypass kube scheduler, meaning we can't preempt existing pods.
By using nodeSelector, we still use kube scheduler and with the right priority class for the agent (default in operator chart now) we always get scheduled.
User might not have "get" access on node, but operator always has so that's why we have a fallback.
