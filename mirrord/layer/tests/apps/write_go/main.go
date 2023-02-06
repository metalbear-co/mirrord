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
	err = file.Close()
	if err != nil {
		panic(err)
	}
}
