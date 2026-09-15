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

func main() {
	queue := os.Getenv("BULLMQ_QUEUE")
	if queue == "" {
		log.Fatal("BULLMQ_QUEUE must be set")
	}

	redisURL := os.Getenv("REDIS_URL")
	if redisURL == "" {
		redisURL = "redis://127.0.0.1:6379"
	}

	prefix := os.Getenv("BULLMQ_PREFIX")
	if prefix == "" {
		prefix = "bull"
	}

	opts, err := redis.ParseURL(redisURL)
	if err != nil {
		log.Fatalf("Failed to parse REDIS_URL: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	client := redis.NewClient(opts)
	defer client.Close()

	fmt.Fprintf(os.Stderr, "go-bullmq-consumer starting\n")
	fmt.Fprintf(os.Stderr, "  BULLMQ_QUEUE=%s\n", queue)
	fmt.Fprintf(os.Stderr, "  REDIS_URL=%s\n", redisURL)
	fmt.Fprintf(os.Stderr, "  BULLMQ_PREFIX=%s\n", prefix)

	waitKey := fmt.Sprintf("%s:%s:wait", prefix, queue)

	// Signal that we are ready to consume. Tests wait for this line before
	// enqueuing jobs so nothing is lost.
	fmt.Fprintf(os.Stderr, "go-bullmq-consumer ready queue=%s\n", queue)

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigChan
		fmt.Fprintln(os.Stderr, "Received shutdown signal")
		cancel()
	}()

	for {
		select {
		case <-ctx.Done():
			return
		default:
		}

		// BLPOP with a 5-second timeout so we periodically check for shutdown.
		result, err := client.BLPop(ctx, 5*time.Second, waitKey).Result()
		if err != nil {
			if err == redis.Nil || ctx.Err() != nil {
				continue
			}
			fmt.Fprintf(os.Stderr, "BLPOP error: %v\n", err)
			time.Sleep(time.Second)
			continue
		}

		// result[0] is the key, result[1] is the job ID.
		jobID := result[1]
		jobKey := fmt.Sprintf("%s:%s:%s", prefix, queue, jobID)

		data, err := client.HGetAll(ctx, jobKey).Result()
		if err != nil {
			fmt.Fprintf(os.Stderr, "HGETALL error for %s: %v\n", jobKey, err)
			continue
		}

		payload, ok := data["data"]
		if !ok {
			fmt.Fprintf(os.Stderr, "Job %s has no data field\n", jobID)
			continue
		}

		fmt.Fprintf(os.Stderr, "Received job %s from queue %s\n", jobID, queue)
		fmt.Printf("1:%s\n", payload)
	}
}
