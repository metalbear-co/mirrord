package main

import (
	"C"
	"fmt"
	"os"
	"os/signal"
    "syscall"
)

const TEXT = "Pineapples."

// Tests: SYS_read, SYS_openat
func main() {
    sigs := make(chan os.Signal, 1)
    signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)
	dat, err := os.ReadFile("/app/test.txt")
	if err != nil {
		panic(err)
	}
	if string(dat) != TEXT {
		err := fmt.Errorf("Expected %s, got %s", TEXT, string(dat))
		panic(err)
	}
	// Wait for SIGTERM/SIGINT before exiting, to give mirrord time to complete the close detour, since Go returns from
	// `Close` before that.
	<-sigs
}
