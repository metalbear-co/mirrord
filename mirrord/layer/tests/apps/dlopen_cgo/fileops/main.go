package main

import "C"

import (
	"fmt"
	"os"
)

//export ReadFileToString
func ReadFileToString(cpath *C.char) *C.char {
	path := C.GoString(cpath)

	data, err := os.ReadFile(path)
	if err != nil {
		msg := fmt.Sprintf("ReadFileToString error: %v", err)
		return C.CString(msg)
	}

	return C.CString(string(data))
}

func main() {}

