//go:build !windows

package main

import (
	"fmt"
	_ "net"
	"os"
	"syscall"
	"unsafe"
)

// Maps an anonymous page and writes to it. On arm64 the kernel places anonymous mappings well above
// 4 GiB, so a syscall result truncated to 32 bits yields a bogus address and the write faults.
func main() {
	addr, _, errno := syscall.Syscall6(
		syscall.SYS_MMAP,
		0,
		4096,
		syscall.PROT_READ|syscall.PROT_WRITE,
		syscall.MAP_PRIVATE|syscall.MAP_ANON,
		^uintptr(0),
		0,
	)
	if errno != 0 {
		fmt.Fprintf(os.Stderr, "mmap failed: %v\n", errno)
		os.Exit(1)
	}
	fmt.Printf("mmap returned %#x\n", addr)

	*(*byte)(unsafe.Pointer(addr)) = 1
}
