package main

import (
	"C"
	"fmt"
)
import (
	"net"
	"os"
)

func lookup() {
	ips, err := net.LookupIP("google.com")
	if err != nil {
		fmt.Fprintf(os.Stderr, "Could not get IPs: %v\n", err)
		os.Exit(1)
	}
	for _, ip := range ips {
		fmt.Printf("google.com. IN A %s\n", ip.String())
	}
}

func main() {
	fmt.Println(">> testing go dns lookup")

	lookup()
}
