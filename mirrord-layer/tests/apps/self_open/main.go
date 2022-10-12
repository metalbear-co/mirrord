/// Temporal calculates the binary checksum as part of the flow, making it not work if read from the remote pod
/// we don't actually want to open the same binary we are running from the remote
/// so that path should be filtered by the regex.

package main

import (
	"fmt"
	"io"
	"os"
	"strings"
	_ "net/http" // for dynamic linkage on linux..
)

func main() {
	path, err := os.Executable()

	if err != nil {
		panic(err)
	}

	file, err := os.Open(path)

	if err != nil {
		panic(err)
	}

	defer file.Close()
	buffer := new(strings.Builder)
	io.Copy(buffer, file)
	fmt.Println("success", buffer.Len())
}
