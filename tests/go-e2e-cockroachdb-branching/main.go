package main

import (
	"database/sql"
	"log"
	"net/http"
	"os"
	"strings"
	"time"

	_ "github.com/lib/pq"
)

func main() {
	log.SetFlags(log.LstdFlags)
	log.Println("CockroachDB test app starting")

	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		log.Fatal("DATABASE_URL not set")
	}
	log.Printf("DATABASE_URL: %s", dsn)

	// The insecure single-node branch has no TLS, so force `sslmode=disable` when the
	// override did not already carry it.
	if !strings.Contains(dsn, "sslmode=") {
		if strings.Contains(dsn, "?") {
			dsn += "&sslmode=disable"
		} else {
			dsn += "?sslmode=disable"
		}
	}

	db, err := sql.Open("postgres", dsn)
	if err != nil {
		log.Fatalf("Failed to open CockroachDB: %v", err)
	}
	defer db.Close()

	connected := false
	for attempt := 1; attempt <= 30; attempt++ {
		if err = db.Ping(); err == nil {
			connected = true
			break
		}
		log.Printf("Waiting for branch CockroachDB (attempt %d/30): %v", attempt, err)
		time.Sleep(2 * time.Second)
	}
	if !connected {
		log.Fatalf("Could not connect to branch CockroachDB: %v", err)
	}
	log.Println("Connected to branch CockroachDB")

	var count int64
	for attempt := 1; attempt <= 15; attempt++ {
		if err = db.QueryRow("SELECT count(*) FROM events").Scan(&count); err == nil {
			break
		}
		log.Printf("Waiting for branch data (attempt %d/15): %v", attempt, err)
		time.Sleep(1 * time.Second)
	}
	log.Printf("ROW COUNT: %d", count)
	log.Println("Verification complete")

	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("healthy"))
	})
	if err := http.ListenAndServe(":8080", nil); err != nil {
		log.Printf("health server stopped: %v", err)
	}
}
