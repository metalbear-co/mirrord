Fixed flaky `mirroring_with_http` layer test (NodeHTTP case) that could hang until timeout on loaded CI by marking request completion before sending the (discarded) response.
