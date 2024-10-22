package main

import (
	"flag"
	"io/fs"
	_ "net" // to assert dynamic linking
	"os"
)

func main() {
	var path string
	var flags int
	var mode uint

	flag.StringVar(&path, "p", "/tmp/testfile", "Path to the file")
	flag.IntVar(&flags, "f", os.O_RDONLY, "Flags to use when opening the file (as raw integer)")
	flag.UintVar(&mode, "m", 0444, "Mode to use when opening the file (as raw integer)")
	flag.Parse()

	fd, err := os.OpenFile(path, flags, fs.FileMode(mode))
	if err != nil {
		panic(err)
	}
	fd.Close()
}
