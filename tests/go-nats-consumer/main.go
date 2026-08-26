package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"os/signal"
	"sort"
	"syscall"
	"time"

	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"
)

func main() {
	natsURL := os.Getenv("NATS_URL")
	if natsURL == "" {
		log.Fatal("NATS_URL must be set")
	}

	streamName := os.Getenv("NATS_STREAM")
	if streamName == "" {
		log.Fatal("NATS_STREAM must be set")
	}

	consumerName := os.Getenv("NATS_CONSUMER")
	if consumerName == "" {
		log.Fatal("NATS_CONSUMER must be set")
	}

	printHeaders := os.Getenv("NATS_TEST_PRINT_HEADERS") == "1"

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	fmt.Fprintf(os.Stderr, "go-nats-consumer starting\n")
	fmt.Fprintf(os.Stderr, "  NATS_URL=%s\n", natsURL)
	fmt.Fprintf(os.Stderr, "  NATS_STREAM=%s\n", streamName)
	fmt.Fprintf(os.Stderr, "  NATS_CONSUMER=%s\n", consumerName)

	nc, err := nats.Connect(natsURL)
	if err != nil {
		log.Fatalf("Failed to connect to NATS: %v", err)
	}
	defer nc.Close()

	js, err := jetstream.New(nc)
	if err != nil {
		log.Fatalf("Failed to create JetStream context: %v", err)
	}

	// The e2e setup pre-creates the durable pull consumer, so binding is the
	// normal path. The create fallback keeps the app usable standalone in the
	// sandbox where nothing pre-creates it.
	cons, err := js.Consumer(ctx, streamName, consumerName)
	if err != nil {
		fmt.Fprintf(os.Stderr, "Bind to consumer %s failed (%v), creating it\n", consumerName, err)
		cons, err = js.CreateOrUpdateConsumer(ctx, streamName, jetstream.ConsumerConfig{
			Durable:   consumerName,
			AckPolicy: jetstream.AckExplicitPolicy,
		})
		if err != nil {
			log.Fatalf("Failed to create consumer %s on stream %s: %v", consumerName, streamName, err)
		}
	}

	fmt.Fprintf(os.Stderr, "Consuming from consumer %s on stream %s\n", consumerName, streamName)

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigChan
		fmt.Fprintln(os.Stderr, "Received shutdown signal")
		cancel()
	}()

	for {
		if ctx.Err() != nil {
			return
		}

		batch, err := cons.Fetch(10, jetstream.FetchMaxWait(time.Second))
		if err != nil {
			if ctx.Err() != nil {
				return
			}
			log.Fatalf("Fetch error: %v", err)
		}

		for msg := range batch.Messages() {
			fmt.Fprintf(os.Stderr, "Received message: subject=%s data_len=%d\n", msg.Subject(), len(msg.Data()))
			fmt.Printf("1:%s\n", string(msg.Data()))
			if printHeaders {
				headers := msg.Headers()
				for _, key := range sortedKeys(headers) {
					for _, value := range headers[key] {
						fmt.Printf("1:hdr:%s=%s\n", key, value)
					}
				}
			}
			if err := msg.Ack(); err != nil {
				fmt.Fprintf(os.Stderr, "Ack failed: %v\n", err)
			}
		}

		// An empty batch with no error is just the FetchMaxWait timeout, so
		// only real errors are surfaced. The nats client reconnects on its
		// own, so a transient fetch error is logged and the loop retries.
		if err := batch.Error(); err != nil {
			fmt.Fprintf(os.Stderr, "Fetch batch error: %v\n", err)
		}
	}
}

func sortedKeys(m map[string][]string) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
