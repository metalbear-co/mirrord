package main

import (
	"C"
	"os"
)

const TEXT = "Pineapples."

func main() {
	file, err := os.Create("/app/test.txt")
	if err != nil {
		panic(err)
	}
	file.WriteString(TEXT)
	// Not closing the file here because Go does not wait for the detour to complete before returning here from `Close`,
	// So the Close message might not be sent by the layer before the app exits. So in order to keep this test
	// deterministic, we don't close here. There is a dedicated close test for testing the close hook.
}
