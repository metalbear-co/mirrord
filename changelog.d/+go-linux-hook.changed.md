Tweaked the Go 1.25+ syscall hook's stack switch to mimic the runtime's `asmcgocall` contract, 
avoiding mutation of goroutine scheduler state that may cause intermittent crashes.
