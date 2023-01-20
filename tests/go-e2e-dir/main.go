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
	if len(dir) != 2 {
		os.Exit(-1)
	}
	// `os.ReadDir` sorts the result by file name.
	if dir[0].Name() != "app.py" || dir[1].Name() != "test.txt"{
		os.Exit(-1)
	}
	// let close requests be sent for test
	time.Sleep(1 * time.Second)
	os.Exit(0)
}
