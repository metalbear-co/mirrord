Route the mirrord CLI build job's downloads through a local caching HTTP proxy backed by the
Actions cache, so repeat runs fetch toolchains and packages from cache instead of the network.
