Added a `replace` service mode to `mirrord up`. A service run in `repalce` mode copies
the target workload and scales the original down to zero. Set it per service with 
`default_mode: replace`, or for a whole run with `mirrord up --mode replace`.
