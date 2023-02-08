package main

import (
	"C"
	"syscall"
)

const TEXT = "Pineapples."

func main() {
	// Tests: SYS_faccessat
	// Access calls Faccess with _AT_FDCWD and flags set to 0
	err := syscall.Access("/app/test.txt", syscall.O_RDWR)
	if err != nil {
		panic(err)
	}
}
