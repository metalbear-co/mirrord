package main

import (
	"C"
	"syscall"
)

func main() {
	syscall.Splice(0, nil,2,nil,4,5)
}
