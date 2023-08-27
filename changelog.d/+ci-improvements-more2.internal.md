Reorganize the CI with the following objective of unifying as much as we can CI that can run on the same host, this is to have less caches and have better compilation time (as there's overlap). Things done:

- Remove the build layer CI, since we now have an integration tests that check it + clippy for aarch darwin / Linux
- Make clippy run for all of the project for aarch64 linux instead of agent only
- Revert removal of Rust cache from e2e (was by mistake)
- Don't use "cache" for other Gos since it will try to overwrite and have bad results.