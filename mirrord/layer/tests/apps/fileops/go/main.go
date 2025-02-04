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

	// statfs/fstatfs hooks are currently disabled in Go apps due to a [bug](https://github.com/metalbear-co/mirrord/issues/3044).

	// var statfs syscall.Statfs_t
	// err = syscall.Statfs(tempFile, &statfs)
	// if err != nil {
	// 	panic(err)
	// }

	// err = syscall.Fstatfs(fd, &statfs)
	// if err != nil {
	// 	panic(err)
	// }
}
