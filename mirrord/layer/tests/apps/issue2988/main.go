package main

import (
	_ "net"
	"os"
)

func main() {
	_, err := os.FindProcess(os.Getpid())
	if err != nil {
		panic(err)
	}
}
