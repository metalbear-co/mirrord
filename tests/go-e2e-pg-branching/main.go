package main

import (
	"database/sql"
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

	_ "github.com/lib/pq"
)

func requireEnv(name string) string {
	val := os.Getenv(name)
	if val == "" {
		log.Fatalf("Required environment variable %s is not set", name)
	}
	return val
}

func buildConnStringFromParams() string {
	host := requireEnv("DB_HOST")
	port := requireEnv("DB_PORT")
	user := requireEnv("DB_USER")
	password := requireEnv("DB_PASSWORD")
	dbname := requireEnv("DB_NAME")

	return fmt.Sprintf("host=%s port=%s user=%s password=%s dbname=%s sslmode=disable",
		host, port, user, password, dbname)
}

// buildConnStringFromServer parses a composite env var and combines the
// extracted host/port with DB_USER, DB_PASSWORD, DB_NAME to build a connection
// string. Supports both URL format ("postgresql://host:5432/db") and plain
// "host:port".
func buildConnStringFromServer(envName string) string {
	server := requireEnv(envName)
	var host, port string
	if strings.Contains(server, "://") {
		u, err := url.Parse(server)
		if err != nil {
			log.Fatalf("Failed to parse %s=%q: %v", envName, server, err)
		}
		host = u.Hostname()
		port = u.Port()
	} else {
		parts := strings.SplitN(server, ":", 2)
		if len(parts) == 2 {
			host = parts[0]
			port = parts[1]
		}
	}
	if host == "" || port == "" {
		log.Fatalf("%s must contain host:port, got %q", envName, server)
	}
	user := requireEnv("DB_USER")
	password := requireEnv("DB_PASSWORD")
	dbname := requireEnv("DB_NAME")
	connStr := fmt.Sprintf("host=%s port=%s user=%s password=%s dbname=%s sslmode=disable",
		host, port, user, password, dbname)
	log.Printf("Using %s mode: %s", envName, connStr)
	return connStr
}

// assertEnvEquals fails if the two env vars have different values.
func assertEnvEquals(a, b string) {
	va, vb := os.Getenv(a), os.Getenv(b)
	if va == "" {
		log.Fatalf("Assertion failed: %s is not set", a)
	}
	if vb == "" {
		log.Fatalf("Assertion failed: %s is not set", b)
	}
	if va != vb {
		log.Fatalf("Assertion failed: %s (%q) != %s (%q)", a, va, b, vb)
	}
	log.Printf("Verified %s == %s", a, b)
}

func main() {
	log.Println("PostgreSQL test app starting")

	var connStr string

	// When TEST_MODE is set (via mirrord env.override), use it to pick the
	// connection mode explicitly. This avoids fragile env-var presence detection
	// and lets multi-source tests verify that all overridden vars match.
	mode := os.Getenv("TEST_MODE")
	switch mode {
	case "url":
		connStr = requireEnv("DATABASE_URL")
		log.Printf("Using DATABASE_URL: %s", connStr)

	case "multi_url":
		connStr = requireEnv("DATABASE_WRITE_URL")
		log.Printf("Using DATABASE_WRITE_URL: %s", connStr)
		assertEnvEquals("DATABASE_WRITE_URL", "DATABASE_READ_URL")
		log.Println("All multi-source env vars verified")

	case "params":
		connStr = buildConnStringFromParams()
		log.Printf("Using params mode: %s", connStr)

	case "composite":
		connStr = buildConnStringFromServer("DB_SERVER")

	case "multi_composite":
		connStr = buildConnStringFromServer("WRITE_SERVER")
		assertEnvEquals("WRITE_SERVER", "READ_SERVER")
		assertEnvEquals("DB_USER", "DB_READ_USER")
		assertEnvEquals("DB_PASSWORD", "DB_READ_PASSWORD")
		log.Println("All multi-source env vars verified")

	case "":
		// Fallback for older test scenarios that don't set TEST_MODE.
		if url := os.Getenv("DATABASE_URL"); url != "" {
			connStr = url
			log.Printf("Using DATABASE_URL: %s", connStr)
		} else if os.Getenv("DB_HOST") != "" {
			connStr = buildConnStringFromParams()
			log.Printf("Using params mode: %s", connStr)
		} else {
			log.Fatal("No supported connection env var is set (set TEST_MODE for new scenarios)")
		}

	default:
		log.Fatalf("Unknown TEST_MODE: %q", mode)
	}

	if !strings.Contains(connStr, "sslmode=") && !strings.Contains(connStr, "sslmode ") {
		if strings.Contains(connStr, "?") {
			connStr += "&sslmode=disable"
		} else if strings.HasPrefix(connStr, "postgres") {
			connStr += "?sslmode=disable"
		}
	}

	db, err := sql.Open("postgres", connStr)
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
