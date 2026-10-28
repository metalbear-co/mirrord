mirrord no longer waits for many minutes, printing nothing, when the cluster answers 503 for the mirrord operator. `startup_retry` alone now decides how often those requests are retried.
