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

	// webClient := http.Client{
	// 	Transport: &http.Transport{
	// 		Proxy: http.ProxyFromEnvironment,
	// 		DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
	// 			dialer := net.Dialer{}
	// 			return dialer.DialContext(ctx, "tcp6", addr)
	// 		},
	// 		MaxIdleConns:          100,
	// 		IdleConnTimeout:       90 * time.Second,
	// 		TLSHandshakeTimeout:   10 * time.Second,
	// 		ExpectContinueTimeout: 1 * time.Second,
	// 	},
	// }

	// res, err := webClient.Get("http://www.google.com/robots.txt")
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
