Requesting a database branch whose engine is disabled on the mirrord operator now fails with a
clear "not enabled on the mirrord operator" message, instead of an opaque `404 page not found` or a
hang waiting for the branch to become ready.
