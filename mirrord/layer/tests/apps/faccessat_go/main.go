package main

import (
	"C"
	"fmt"
	"os"
	"syscall"
)

const TEXT = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat cupidatat non proident, sunt in culpa qui officia deserunt mollit anim id est laborum."

func createTempFile() string {
	file, err := os.CreateTemp("/tmp", "test")
	if err != nil {
		panic(err)
	}
	file.WriteString(TEXT)
	fileName := file.Name()
	file.Close()
	return fileName
}

func main() {
	// Tests: SYS_faccessat
	// Access calls Faccess with _AT_FDCWD and flags set to 0
	fileName := createTempFile()
	err := syscall.Access(fileName, syscall.O_RDWR)
	if err != nil {
		panic(err)
	}
}
