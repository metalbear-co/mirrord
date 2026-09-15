// Local consumer for the multi-broker queue-splitting e2e test. It runs under mirrord against the
// multi-broker workload, which exposes SQS, Redis Pub/Sub and Temporal at once. Of those, Redis is
// the one a local process can consume with no extra credentials, so this app subscribes to the
// workload's Redis channel and prints each matched message.
//
// It also pings Redis on a short interval. mirrord only learns the operator closed the session on
// the next hooked syscall the process makes, so a consumer that merely blocks on a subscription
// would hang when the params-change watcher fails the session. The periodic ping guarantees an
// outgoing call every tick, so the session-closed signal reaches the layer and `mirrord exec`
// exits promptly.
package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/redis/go-redis/v9"
)

const pingInterval = time.Second

func main() {
	channel := os.Getenv("REDIS_CHANNEL")
	if channel == "" {
		log.Fatal("REDIS_CHANNEL must be set")
	}

	redisURL := os.Getenv("REDIS_URL")
	if redisURL == "" {
		log.Fatal("REDIS_URL must be set")
	}

	opts, err := redis.ParseURL(redisURL)
	if err != nil {
		log.Fatalf("Failed to parse REDIS_URL: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	client := redis.NewClient(opts)
	defer client.Close()

	fmt.Fprintf(os.Stderr, "go-multi-broker-consumer starting\n")
	fmt.Fprintf(os.Stderr, "  REDIS_CHANNEL=%s\n", channel)
	fmt.Fprintf(os.Stderr, "  REDIS_URL=%s\n", redisURL)

	pubsub := client.Subscribe(ctx, channel)
	defer pubsub.Close()

	// Receive blocks until Redis confirms the SUBSCRIBE, so once it returns the subscription is
	// live. Print a marker so the test can wait for this point before publishing.
	if _, err := pubsub.Receive(ctx); err != nil {
		log.Fatalf("Subscribe failed: %v", err)
	}
	fmt.Fprintf(os.Stderr, "go-multi-broker-consumer subscribed channel=%s\n", channel)

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigChan
		fmt.Fprintln(os.Stderr, "Received shutdown signal")
		cancel()
	}()

	ticker := time.NewTicker(pingInterval)
	defer ticker.Stop()

	ch := pubsub.Channel()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			// A failed ping means the connection is gone. Under mirrord that happens when the
			// operator closes the session, so the dial behind the next ping triggers mirrord's
			// graceful exit and this process ends.
			if err := client.Ping(ctx).Err(); err != nil {
				log.Fatalf("Redis ping failed: %v", err)
			}
		case msg, ok := <-ch:
			if !ok {
				return
			}
			fmt.Fprintf(os.Stderr, "Received message on %s: len=%d\n", msg.Channel, len(msg.Payload))
			fmt.Printf("1:%s\n", msg.Payload)
		}
	}
}
