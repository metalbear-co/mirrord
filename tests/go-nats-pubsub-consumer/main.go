package main

import (
	"fmt"
	"log"
	"net/url"
	"os"
	"os/signal"
	"sort"
	"syscall"
	"time"

	"github.com/nats-io/nats.go"
)

// The URL can carry credentials in its userinfo part, and test output ends up
// retained in CI and pod logs, so only a redacted form is ever printed.
func redactURL(raw string) string {
	parsed, err := url.Parse(raw)
	if err != nil {
		return "<unparseable>"
	}
	if parsed.User != nil {
		parsed.User = url.User("REDACTED")
	}
	return parsed.Redacted()
}

func main() {
	natsURL := os.Getenv("NATS_URL")
	if natsURL == "" {
		log.Fatal("NATS_URL must be set")
	}

	subject := os.Getenv("NATS_SUBJECT")
	if subject == "" {
		log.Fatal("NATS_SUBJECT must be set")
	}

	printHeaders := os.Getenv("NATS_TEST_PRINT_HEADERS") == "1"

	fmt.Fprintf(os.Stderr, "go-nats-pubsub-consumer starting\n")
	fmt.Fprintf(os.Stderr, "  NATS_URL=%s\n", redactURL(natsURL))
	fmt.Fprintf(os.Stderr, "  NATS_SUBJECT=%s\n", subject)

	// The server may come up after the pod; core NATS has no queue to buffer
	// through the wait, so just keep retrying until the connection lands.
	var nc *nats.Conn
	var err error
	for {
		nc, err = nats.Connect(natsURL, nats.RetryOnFailedConnect(true), nats.MaxReconnects(-1))
		if err == nil {
			break
		}
		fmt.Fprintf(os.Stderr, "Connect failed (%v), retrying\n", err)
		time.Sleep(2 * time.Second)
	}
	defer nc.Close()

	messages := make(chan *nats.Msg, 64)
	sub, err := nc.ChanSubscribe(subject, messages)
	if err != nil {
		log.Fatalf("Failed to subscribe to %s: %v", subject, err)
	}
	defer func() {
		_ = sub.Unsubscribe()
	}()

	fmt.Fprintf(os.Stderr, "Subscribed to %s\n", subject)

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)

	for {
		select {
		case <-sigChan:
			fmt.Fprintln(os.Stderr, "Received shutdown signal")
			return
		case msg := <-messages:
			fmt.Fprintf(os.Stderr, "Received message: subject=%s data_len=%d\n", msg.Subject, len(msg.Data))
			fmt.Printf("1:%s\n", string(msg.Data))
			if printHeaders {
				keys := make([]string, 0, len(msg.Header))
				for key := range msg.Header {
					keys = append(keys, key)
				}
				sort.Strings(keys)
				for _, key := range keys {
					for _, value := range msg.Header[key] {
						fmt.Printf("1:hdr:%s=%s\n", key, value)
					}
				}
			}
		}
	}
}
