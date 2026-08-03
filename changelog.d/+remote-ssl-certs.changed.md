`/etc/ssl/certs` is now read from the remote target by default, so the local process trusts the same
certificate authorities as the target when talking to services in the cluster. Add the path to
`feature.fs.local` to restore the previous behaviour.
