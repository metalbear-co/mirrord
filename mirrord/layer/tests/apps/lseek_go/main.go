package main

import (
	"C"
	"fmt"
	"os"
)

const TEXT = "Pineapples."

// Tests: SYS_lseek
func main() {
    fileName := "/app/test.txt"
	file, err := os.Open(fileName)
	if err != nil {
		panic(err)
	}
	buf := make([]byte, len(TEXT))
	var offset int64 = 4
	file.Seek(offset, 1) // 1 means the offset is from the start.
	amount_read, err := file.Read(buf)
	if err != nil {
		panic(err)
	}
	if string(buf[:amount_read]) != TEXT[offset:] {
		err := fmt.Errorf("Expected %s, got %s", TEXT[offset:], string(buf))
		panic(err)
	}
	err = file.Close()
	if err != nil {
		panic(err)
	}
}
