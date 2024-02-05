Added `runAsNonRoot: false` and `runAsUser: 0` to the security context of an epheremal agent when running privileged (to prevent overriding these values with values from the pod spec).
