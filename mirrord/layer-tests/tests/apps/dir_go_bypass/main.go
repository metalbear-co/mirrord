package main

import (
	"fmt"
	"os"

	"C"
)
import "time"

func main() {
    dirPath := os.Getenv("MIRRORD_TEST_GO_DIR_BYPASS_PATH")
	dir, err := os.ReadDir(dirPath)
	if err != nil {
		fmt.Printf("Reading dir error: %s\n", err)
		os.Exit(-1)
	}
	fmt.Printf("DirEntries: %s\n", dir)
	if len(dir) != 2 {
		os.Exit(-1)
	}
	if dir[0].Name() != "a" || dir[1].Name() != "b" {
		os.Exit(-1)
	}
	// let close requests be sent for test
	time.Sleep(1 * time.Second)
	os.Exit(0)
}
