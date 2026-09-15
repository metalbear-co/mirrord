package main

import (
	"database/sql"
	"log"
	"net/http"
	"os"
	"time"

	_ "github.com/ClickHouse/clickhouse-go/v2"
)

func main() {
	log.SetFlags(log.LstdFlags)
	log.Println("ClickHouse test app starting")

	dsn := os.Getenv("DATABASE_URL")
	if dsn == "" {
		log.Fatal("DATABASE_URL not set")
	}
	log.Printf("DATABASE_URL: %s", dsn)

	db, err := sql.Open("clickhouse", dsn)
	if err != nil {
		log.Fatalf("Failed to open ClickHouse: %v", err)
	}
	defer db.Close()

	connected := false
	for attempt := 1; attempt <= 30; attempt++ {
		if err = db.Ping(); err == nil {
			connected = true
			break
		}
		log.Printf("Waiting for branch ClickHouse (attempt %d/30): %v", attempt, err)
		time.Sleep(2 * time.Second)
	}
	if !connected {
		log.Fatalf("Could not connect to branch ClickHouse: %v", err)
	}
	log.Println("Connected to branch ClickHouse")

	var count int64
	for attempt := 1; attempt <= 15; attempt++ {
		if err = db.QueryRow("SELECT count() FROM source_db.events").Scan(&count); err == nil {
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
