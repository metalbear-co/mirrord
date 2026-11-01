The Temporal worker and Redis Pub/Sub consumer test apps bound each connection attempt by the retry limit, so a hanging attempt cannot outlive it.
