package main

import (
	"C"
	"syscall"
)

func main() {
	tempFile := "/tmp/test_file.txt"
	syscall.Open(tempFile, syscall.O_CREAT|syscall.O_WRONLY, 0644)
	var stat syscall.Stat_t
	err := syscall.Stat(tempFile, &stat)
	if err != nil {
		panic(err)
	}
}
