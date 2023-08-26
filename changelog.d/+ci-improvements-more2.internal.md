Reorganize the CI with the following objective of unifying as much as we can CI that can run on the same host, this is to have less caches and have better compilation time (as there's overlap). Things done

- Remove the build layer CI, since we now have an integration tests that check it + clippy for aarch darwin / Linux