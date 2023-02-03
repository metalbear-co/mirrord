package main

import (
	"C"
	"fmt"
	"os"
)

const TEXT = "Pineapples."

func main() {
	// Tests: SYS_read, SYS_openat
	dat, err := os.ReadFile("/app/test.txt")
	if err != nil {
		panic(err)
	}
	if string(dat) != TEXT {
		err := fmt.Errorf("Expected %s, got %s", TEXT, string(dat))
		panic(err)
	}
}
