package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"os/signal"
	"sort"
	"syscall"

	"cloud.google.com/go/pubsub"
)

func main() {
	projectID := os.Getenv("PUBSUB_PROJECT_ID")
	if projectID == "" {
		log.Fatal("PUBSUB_PROJECT_ID must be set")
	}

	subscriptionID := os.Getenv("PUBSUB_SUBSCRIPTION")
	if subscriptionID == "" {
		log.Fatal("PUBSUB_SUBSCRIPTION must be set")
	}

	_, printAttrs := os.LookupEnv("PUBSUB_TEST_PRINT_ATTRS")

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	client, err := pubsub.NewClient(ctx, projectID)
	if err != nil {
		log.Fatalf("Failed to create Pub/Sub client: %v", err)
	}
	defer client.Close()

	emulatorHost := os.Getenv("PUBSUB_EMULATOR_HOST")
	fmt.Fprintf(os.Stderr, "go-gcp-pubsub-consumer starting\n")
	fmt.Fprintf(os.Stderr, "  PUBSUB_PROJECT_ID=%s\n", projectID)
	fmt.Fprintf(os.Stderr, "  PUBSUB_SUBSCRIPTION=%s\n", subscriptionID)
	fmt.Fprintf(os.Stderr, "  PUBSUB_EMULATOR_HOST=%s\n", emulatorHost)

	sub := client.Subscription(subscriptionID)

	fmt.Fprintf(os.Stderr, "Subscribing to %s in project %s\n", subscriptionID, projectID)

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigChan
		fmt.Fprintln(os.Stderr, "Received shutdown signal")
		cancel()
	}()

	fmt.Fprintf(os.Stderr, "Starting receive loop on subscription %s\n", subscriptionID)
	err = sub.Receive(ctx, func(_ context.Context, msg *pubsub.Message) {
		fmt.Fprintf(os.Stderr, "Received message: id=%s data_len=%d attrs=%v\n", msg.ID, len(msg.Data), msg.Attributes)
		fmt.Printf("1:%s\n", string(msg.Data))
		if printAttrs {
			for _, key := range sortedKeys(msg.Attributes) {
				fmt.Printf("1:attr:%s=%s\n", key, msg.Attributes[key])
			}
		}
		msg.Ack()
	})
	if err != nil && ctx.Err() == nil {
		log.Fatalf("Receive error: %v", err)
	}
}

func sortedKeys(m map[string]string) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
