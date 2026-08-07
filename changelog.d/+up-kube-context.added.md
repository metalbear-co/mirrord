Added support for specifying a kube context in `mirrord up`. In order of precedence, it can be set:

1. with the `--context` argument when running `mirrord up` (highest precedence)
2. with the `context` field under a service in the configuration file
3. with the `common.context` field in the configuration file

If none of these are set, the default behaviour remains the same.
