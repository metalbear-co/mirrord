package main

import (
	"fmt"
	"os"

	"C"
)
import (
	"time"

	"golang.org/x/sys/unix"
)

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

	// Regression test for `fstatat(dirfd, name, ...)` against a directory fd opened through
	// mirrord. `ioutil.ReadDir` and `(*os.File).Readdir` stat each entry this way, and on the
	// agent side `open` stores an already-resolved path for the directory fd, so looking up an
	// entry through it must not be resolved against the target's root a second time.
	dirFile, err := os.Open("/app")
	if err != nil {
		fmt.Printf("Open dir error: %s\n", err)
		os.Exit(-1)
	}
	dirFd := int(dirFile.Fd())
	for i := 0; i < len(dir); i++ {
		name := dir[i].Name()
		var stat unix.Stat_t
		if err := unix.Fstatat(dirFd, name, &stat, unix.AT_SYMLINK_NOFOLLOW); err != nil {
			fmt.Printf("Fstatat error for %s: %s\n", name, err)
			os.Exit(-1)
		}
	}
	dirFile.Close()

	err = os.Mkdir("/app/test_mkdir", 0755)
	if err != nil {
		fmt.Printf("Mkdir error: %s\n", err)
		os.Exit(-1)
	}

	err = os.Remove("/app/test_mkdir")
	if err != nil {
		fmt.Printf("Rmdir error: %s\n", err)
		os.Exit(-1)
	}

	// let close requests be sent for test
	time.Sleep(1 * time.Second)
	os.Exit(0)
}
