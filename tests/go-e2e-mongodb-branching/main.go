package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"go.mongodb.org/mongo-driver/bson"
	"go.mongodb.org/mongo-driver/mongo"
	"go.mongodb.org/mongo-driver/mongo/options"
)

func init() {
	// Ensure output is not buffered
	log.SetFlags(log.LstdFlags)
}

func extractDatabaseName(mongoURL string) string {
	parsed, err := url.Parse(mongoURL)
	if err != nil {
		return "test"
	}
	path := strings.TrimPrefix(parsed.Path, "/")
	if idx := strings.Index(path, "?"); idx != -1 {
		path = path[:idx]
	}
	if path == "" {
		return "test"
	}
	return path
}

func main() {
	// Print immediately to stderr
	fmt.Fprintln(os.Stderr, "MongoDB test app starting")
	log.Println("MongoDB test app starting")

	dbURL := os.Getenv("DATABASE_URL")
	if dbURL == "" {
		log.Fatal("DATABASE_URL not set")
	}

	log.Printf("DATABASE_URL: %s", dbURL)

	dbName := extractDatabaseName(dbURL)
	log.Printf("Using database: %s", dbName)

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	clientOpts := options.Client().ApplyURI(dbURL)
	client, err := mongo.Connect(ctx, clientOpts)
	if err != nil {
		log.Fatalf("Failed to connect to MongoDB: %v", err)
	}
	defer client.Disconnect(ctx)

	// Retry ping (branch DB may be initializing)
	for i := 0; i < 15; i++ {
		pingCtx, pingCancel := context.WithTimeout(context.Background(), 5*time.Second)
		if err = client.Ping(pingCtx, nil); err == nil {
			pingCancel()
			break
		}
		pingCancel()
		log.Printf("Waiting for DB (attempt %d/15): %v", i+1, err)
		time.Sleep(2 * time.Second)
	}
	if err != nil {
		log.Fatalf("DB connection failed: %v", err)
	}

	log.Println("Connected to database")

	db := client.Database(dbName)

	// Wait for data to appear (init scripts may still be running)
	collections := []string{"users", "orders", "products"}
	maxWaitAttempts := 30
	for attempt := 1; attempt <= maxWaitAttempts; attempt++ {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		usersCount, err := db.Collection("users").CountDocuments(ctx, bson.M{})
		cancel()
		if err == nil && usersCount > 0 {
			log.Printf("Data found after %d attempts", attempt)
			break
		}
		if attempt == maxWaitAttempts {
			log.Printf("Warning: No data found after %d attempts, proceeding anyway", maxWaitAttempts)
		} else {
			log.Printf("Waiting for data (attempt %d/%d): users=%d", attempt, maxWaitAttempts, usersCount)
			time.Sleep(1 * time.Second)
		}
	}

	// Verify database has expected collections
	for _, coll := range collections {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		count, err := db.Collection(coll).CountDocuments(ctx, bson.M{})
		cancel()
		if err != nil {
			log.Printf("Collection %s: query error (%v)", coll, err)
		} else {
			log.Printf("Collection %s: %d documents", coll, count)
		}
	}

	log.Println("Database verification complete")

	// Start HTTP server for health checks
	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		result := make(map[string]interface{})
		result["status"] = "healthy"
		result["collections"] = make(map[string]int64)

		allOk := true
		for _, coll := range collections {
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			count, err := db.Collection(coll).CountDocuments(ctx, bson.M{})
			cancel()
			if err != nil {
				log.Printf("Health check: Collection %s error: %v", coll, err)
				result["collections"].(map[string]int64)[coll] = -1
				allOk = false
			} else {
				result["collections"].(map[string]int64)[coll] = count
			}
		}

		if !allOk {
			result["status"] = "unhealthy"
			w.WriteHeader(http.StatusInternalServerError)
		} else {
			w.WriteHeader(http.StatusOK)
		}

		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(result)
	})

	server := &http.Server{Addr: ":8080"}
	go func() {
		log.Println("Starting HTTP server on :8080")
		if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			log.Printf("HTTP server error: %v", err)
		}
	}()

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)
	log.Println("App running, press Ctrl+C to stop")
	<-sigChan

	log.Println("Shutting down")
}
