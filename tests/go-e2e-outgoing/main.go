package main

import (
	"C"
	"fmt"
	"io"
	"log"
	"net/http"
)
import (
	"net"
	"os"
)

func request() {
	fmt.Println(">> making request")

	res, err := http.Get("http://www.google.com/robots.txt")
	if err != nil {
		log.Fatal(err)
	}

	body, err := io.ReadAll(res.Body)
	res.Body.Close()

	if res.StatusCode > 299 {
		log.Fatalf("Response failed with status code: %d and\nbody: %s\n", res.StatusCode, body)
	}

	if err != nil {
		log.Fatal(err)
		panic(err)
	}

	fmt.Printf("body: %s", body)
	fmt.Printf("done")
}

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
	fmt.Println(">> starting go")

	request()
	// lookup()
}
