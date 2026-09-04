// Exercises outgoing traffic from Go's runtime network path: an in-cluster DNS
// lookup and TCP dial, followed by TLS handshakes against external hosts.
//
// The handshakes are the interesting part - a plain HTTP request over an
// outgoing connection is already covered by `go-e2e-outgoing`, while a TLS
// handshake additionally depends on the connection carrying the full
// negotiation, including ALPN and a multi-certificate chain, before any
// application data flows.
//
// `CLUSTER_TARGET` ("host:port", reachable from the mirrord target) enables the
// in-cluster probes. Without it, only the external handshakes run.
package main

import (
	"crypto/tls"
	"fmt"
	"net"
	"os"
	"strings"
	"time"
)

const timeout = 10 * time.Second

// Runs fn, reports how it went, and returns whether it succeeded.
func probe(name string, fn func() error) bool {
	start := time.Now()
	err := fn()
	fmt.Printf("%-12s err=%v secs=%.2f\n", name, err, time.Since(start).Seconds())
	return err == nil
}

func tlsHandshake(address string) func() error {
	return func() error {
		dialer := &net.Dialer{Timeout: timeout}
		conn, err := tls.DialWithDialer(dialer, "tcp", address, nil)
		if err != nil {
			return err
		}
		return conn.Close()
	}
}

func main() {
	ok := true

	if cluster := os.Getenv("CLUSTER_TARGET"); cluster != "" {
		host := strings.Split(cluster, ":")[0]

		ok = probe("dns_cluster", func() error {
			_, err := net.LookupHost(host)
			return err
		}) && ok

		ok = probe("tcp_cluster", func() error {
			conn, err := net.DialTimeout("tcp", cluster, timeout)
			if err != nil {
				return err
			}
			return conn.Close()
		}) && ok
	}

	// Both hosts are probed so that a failure specific to one endpoint is
	// distinguishable from outgoing TLS being broken altogether.
	ok = probe("tls_spanner", tlsHandshake("spanner.googleapis.com:443")) && ok
	ok = probe("tls_google", tlsHandshake("www.google.com:443")) && ok

	if !ok {
		os.Exit(1)
	}
}
