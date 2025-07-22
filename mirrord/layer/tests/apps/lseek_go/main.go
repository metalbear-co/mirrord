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
	// Not closing the file here because Go does not wait for the detour to complete before returning here from `Close`,
	// So the Close message might not be sent by the layer before the app exits. So in order to keep this test
	// deterministic, we don't close here. There is a dedicated close test for testing the close hook.
}
