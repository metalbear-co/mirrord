package main

import (
	"database/sql"
	"encoding/json"
	"log"
	"net/http"
	"net/url"
	"os"
	"os/signal"
	"syscall"
	"time"

	_ "github.com/go-sql-driver/mysql"
)

// urlToDSN converts a mysql:// URL to Go MySQL driver DSN format.
// MariaDB speaks the MySQL wire protocol, so the same driver connects to it.
// mysql://user:pass@host:port/db -> user:pass@tcp(host:port)/db
func urlToDSN(rawURL string) string {
	u, err := url.Parse(rawURL)
	if err != nil {
		return rawURL
	}
	if u.Scheme != "mysql" {
		return rawURL
	}
	password, _ := u.User.Password()
	host := u.Hostname()
	port := u.Port()
	if port == "" {
		port = "3306"
	}
	return u.User.Username() + ":" + password + "@tcp(" + host + ":" + port + ")" + u.Path
}

func main() {
	log.Println("MariaDB test app starting")

	dbURL := os.Getenv("DATABASE_URL")
	if dbURL == "" {
		log.Fatal("DATABASE_URL not set")
	}

	log.Printf("DATABASE_URL: %s", dbURL)

	dsn := urlToDSN(dbURL)
	log.Printf("Connecting with DSN: %s", dsn)

	db, err := sql.Open("mysql", dsn)
	if err != nil {
		log.Fatalf("Failed to open database: %v", err)
	}
	defer db.Close()

	// Retry connection (branch DB may be initializing)
	for i := 0; i < 15; i++ {
		if err = db.Ping(); err == nil {
			break
		}
		log.Printf("Waiting for DB (attempt %d/15)", i+1)
		time.Sleep(2 * time.Second)
	}
	if err != nil {
		log.Fatalf("DB connection failed: %v", err)
	}

	log.Println("Connected to database")

	// Verify database has expected tables
	tables := []string{"users", "orders", "products"}
	for _, table := range tables {
		var count int
		query := "SELECT COUNT(*) FROM " + table
		if err := db.QueryRow(query).Scan(&count); err != nil {
			log.Printf("Table %s: query error (%v)", table, err)
		} else {
			log.Printf("Table %s: %d rows", table, count)
		}
	}

	log.Println("Database verification complete")

	// Start HTTP server for health checks
	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		result := make(map[string]interface{})
		result["status"] = "healthy"
		result["tables"] = make(map[string]int)

		allOk := true
		for _, table := range tables {
			var count int
			query := "SELECT COUNT(*) FROM " + table
			if err := db.QueryRow(query).Scan(&count); err != nil {
				log.Printf("Health check: Table %s error: %v", table, err)
				result["tables"].(map[string]int)[table] = -1
				allOk = false
			} else {
				result["tables"].(map[string]int)[table] = count
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
