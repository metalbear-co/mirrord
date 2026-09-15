package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"

	"github.com/redis/go-redis/v9"
)

func main() {
	channel := os.Getenv("REDIS_CHANNEL")
	if channel == "" {
		log.Fatal("REDIS_CHANNEL must be set")
	}

	redisURL := os.Getenv("REDIS_URL")
	if redisURL == "" {
		redisURL = "redis://127.0.0.1:6379"
	}

	subscribeMode := os.Getenv("REDIS_SUBSCRIBE_MODE")
	if subscribeMode == "" {
		subscribeMode = "exact"
	}

	opts, err := redis.ParseURL(redisURL)
	if err != nil {
		log.Fatalf("Failed to parse REDIS_URL: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	client := redis.NewClient(opts)
	defer client.Close()

	fmt.Fprintf(os.Stderr, "go-redis-pubsub-consumer starting\n")
	fmt.Fprintf(os.Stderr, "  REDIS_CHANNEL=%s\n", channel)
	fmt.Fprintf(os.Stderr, "  REDIS_URL=%s\n", redisURL)
	fmt.Fprintf(os.Stderr, "  REDIS_SUBSCRIBE_MODE=%s\n", subscribeMode)

	var pubsub *redis.PubSub
	switch subscribeMode {
	case "pattern":
		pubsub = client.PSubscribe(ctx, channel)
	case "sharded":
		pubsub = client.SSubscribe(ctx, channel)
	default:
		pubsub = client.Subscribe(ctx, channel)
	}
	defer pubsub.Close()

	// Receive blocks until Redis confirms the (P/S)SUBSCRIBE, so once it returns
	// the subscription is live. Print a marker so tests can wait for this exact
	// point before publishing: Redis Pub/Sub drops messages sent while nobody is
	// subscribed, so publishing any earlier would lose them.
	if _, err := pubsub.Receive(ctx); err != nil {
		log.Fatalf("Subscribe failed: %v", err)
	}
	fmt.Fprintf(os.Stderr, "go-redis-pubsub-consumer subscribed channel=%s\n", channel)

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigChan
		fmt.Fprintln(os.Stderr, "Received shutdown signal")
		cancel()
	}()

	ch := pubsub.Channel()
	for {
		select {
		case <-ctx.Done():
			return
		case msg, ok := <-ch:
			if !ok {
				return
			}
			fmt.Fprintf(os.Stderr, "Received message on %s: len=%d\n", msg.Channel, len(msg.Payload))
			fmt.Printf("1:%s\n", msg.Payload)
		}
	}
}
