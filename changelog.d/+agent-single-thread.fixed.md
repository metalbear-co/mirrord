Change agent to use current thread runtime, multi thread is enabled by mistake.
* Added a sleep before exiting both in layer and agent to allow `tokio::spawn` tasks spawned from `Drop` finish.
* Changed implementation of pause guard to use `tokio::spawn` - fixes pause in combination with above change.