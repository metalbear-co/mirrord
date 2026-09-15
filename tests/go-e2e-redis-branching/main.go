package main

import (
	"context"
	"log"
	"net/http"
	"os"
	"time"

	"github.com/redis/go-redis/v9"
)

func main() {
	log.SetFlags(log.LstdFlags)
	log.Println("Redis test app starting")

	redisURL := os.Getenv("REDIS_URL")

	if redisURL == "" {
		log.Fatal("REDIS_URL not set")
	}

	log.Printf("REDIS_URL: %s", redisURL)

	opts, err := redis.ParseURL(redisURL)

	if err != nil {
		log.Fatalf("Failed to parse REDIS_URL: %v", err)
	}

	client := redis.NewClient(opts)

	defer client.Close()

	ctx := context.Background()

	var dbsize int64

	connected := false

	for attempt := 1; attempt <= 30; attempt++ {
		pingCtx, cancel := context.WithTimeout(ctx, 5*time.Second)

		err = client.Ping(pingCtx).Err()

		cancel()

		if err == nil {
			connected = true
			break
		}

		log.Printf("Waiting for branch Redis (attempt %d/30): %v", attempt, err)

		time.Sleep(2 * time.Second)
	}
	if !connected {
		log.Fatalf("Could not connect to branch Redis: %v", err)
	}

	log.Println("Connected to branch Redis")

	for attempt := 1; attempt <= 15; attempt++ {
		sizeCtx, cancel := context.WithTimeout(ctx, 5*time.Second)

		dbsize, err = client.DBSize(sizeCtx).Result()

		cancel()

		if err == nil && dbsize > 0 {
			break
		}

		log.Printf("Waiting for branch data (attempt %d/15): dbsize=%d err=%v", attempt, dbsize, err)

		time.Sleep(1 * time.Second)
	}

	log.Printf("DBSIZE: %d", dbsize)

	getCtx, cancel := context.WithTimeout(ctx, 5*time.Second)

	if val, err := client.Get(getCtx, "user:1").Result(); err == nil {
		log.Printf("user:1 = %s", val)
	} else {
		log.Printf("user:1 not present (%v)", err)
	}

	cancel()

	log.Println("Verification complete")

	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)

		_, _ = w.Write([]byte("healthy"))
	})

	if err := http.ListenAndServe(":8080", nil); err != nil {
		log.Printf("health server stopped: %v", err)
	}
}
