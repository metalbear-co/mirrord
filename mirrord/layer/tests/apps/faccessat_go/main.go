package main

import (
	"C"
	"syscall"
	"os"
)

func main() {

	// First make a call that should be bypassed.
	localHomeDir, homeDirErr := os.UserHomeDir()
	if homeDirErr != nil {
		panic(homeDirErr)
	}
	localErr := syscall.Access(localHomeDir, syscall.O_RDWR)
	if localErr != nil {
		panic(localErr)
	}

	// Tests: SYS_faccessat
	// Access calls Faccess with _AT_FDCWD and flags set to 0
	err := syscall.Access("/app/test.txt", syscall.O_RDWR)
	if err != nil {
		panic(err)
	}
}
