package main

import (
	"fmt"
	"os"

	"C"
)
import "time"

func main() {
	dir, err := os.ReadDir("/app")
	if err != nil {
		fmt.Printf("Reading dir error: %s\n", err)
		os.Exit(-1)
	}
	fmt.Printf("DirEntries: %s\n", dir)

	// `os.ReadDir` does not include `.` and `..`.
	if len(dir) < 2 {
		os.Exit(-1)
	}

	// Iterate over the files in this dir, exiting if it's not an expected file name.
	for i := 0; i < len(dir); i++ {
		dirName := dir[i].Name()

		if dirName != "app.py" && dirName != "test.txt" && dirName != "file.local" && dirName != "file.not-found" && dirName != "file.read-only" && dirName != "file.read-write" {
			os.Exit(-1)
		}

	}

	err = os.Mkdir("/app/test_mkdir", 0755)
	if err != nil {
		fmt.Printf("Mkdir error: %s\n", err)
		os.Exit(-1)
	}

	// let close requests be sent for test
	time.Sleep(1 * time.Second)
	os.Exit(0)
}
