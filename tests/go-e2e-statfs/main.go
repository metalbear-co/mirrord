//go:build linux

package main

import (
	"encoding/json"
	"fmt"
	_ "net" // for dynamic linking
	"syscall"
)

func main() {
	testPath := "/app/test.txt"
	var statfs syscall.Statfs_t
	err := syscall.Statfs(testPath, &statfs)
	if err != nil {
		fmt.Println("Error:", err)
		return
	}

	// Convert struct to a JSON-friendly format
	data := map[string]interface{}{
		"bavail":  statfs.Bavail,
		"bfree":   statfs.Bfree,
		"blocks":  statfs.Blocks,
		"bsize":   statfs.Bsize,
		"ffree":   statfs.Ffree,
		"files":   statfs.Files,
		"flags":   statfs.Flags,
		"frsize":  statfs.Frsize,
		"fsid":    []int32{int32(statfs.Fsid.X__val[0]), int32(statfs.Fsid.X__val[1])}, // Convert fsid to list
		"namelen": statfs.Namelen,
		"spare":   statfs.Spare,
		"type":    statfs.Type,
	}

	// Convert to JSON
	jsonData, err := json.MarshalIndent(data, "", "  ")
	if err != nil {
		fmt.Println("JSON Encoding Error:", err)
		return
	}

	// Print JSON
	fmt.Println(string(jsonData))
}
