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

	_ "github.com/microsoft/go-mssqldb"
)

// mssqlURLToSqlserver converts mssql://user:pass@host:port/db to
// sqlserver://user:pass@host:port?database=db (Go driver format).
func mssqlURLToSqlserver(rawURL string) string {
	u, err := url.Parse(rawURL)
	if err != nil {
		return rawURL
	}
	if u.Scheme != "mssql" {
		return rawURL
	}
	u.Scheme = "sqlserver"
	dbName := ""
	if len(u.Path) > 1 {
		dbName = u.Path[1:]
	}
	u.Path = ""
	q := u.Query()
	if dbName != "" {
		q.Set("database", dbName)
	}
	q.Set("TrustServerCertificate", "true")
	u.RawQuery = q.Encode()
	return u.String()
}

func main() {
	log.Println("MSSQL test app starting")

	dbURL := os.Getenv("DATABASE_URL")
	if dbURL == "" {
		log.Fatal("DATABASE_URL not set")
	}

	log.Printf("DATABASE_URL: %s", dbURL)

	connStr := mssqlURLToSqlserver(dbURL)
	log.Printf("Connecting with: %s", connStr)

	db, err := sql.Open("sqlserver", connStr)
	if err != nil {
		log.Fatalf("Failed to open database: %v", err)
	}
	defer db.Close()

	for i := 0; i < 30; i++ {
		if err = db.Ping(); err == nil {
			break
		}
		log.Printf("Waiting for DB (attempt %d/30)", i+1)
		time.Sleep(2 * time.Second)
	}
	if err != nil {
		log.Fatalf("DB connection failed: %v", err)
	}

	log.Println("Connected to database")

	tables := []string{"users", "orders", "products"}
	for _, table := range tables {
		var count int
		query := "SELECT COUNT(*) FROM [" + table + "]"
		if err := db.QueryRow(query).Scan(&count); err != nil {
			log.Printf("Table %s: query error (%v)", table, err)
		} else {
			log.Printf("Table %s: %d rows", table, count)
		}
	}

	// Verify filter: only active users should be present
	var activeCount int
	err = db.QueryRow("SELECT COUNT(*) FROM users WHERE active = 1").Scan(&activeCount)
	if err != nil {
		log.Printf("Active users query error: %v", err)
	} else {
		log.Printf("Active users: %d", activeCount)
	}
	var totalUsers int
	err = db.QueryRow("SELECT COUNT(*) FROM users").Scan(&totalUsers)
	if err != nil {
		log.Printf("Total users query error: %v", err)
	}
	if totalUsers > 0 && totalUsers == activeCount {
		log.Println("Filter verification: PASS - all users are active")
	} else if totalUsers == 0 {
		log.Println("Filter verification: SKIP - no user data (schema-only mode)")
	} else {
		log.Printf("Filter verification: FAIL - total=%d active=%d", totalUsers, activeCount)
	}

	log.Println("Database verification complete")

	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		result := make(map[string]interface{})
		result["status"] = "healthy"
		result["tables"] = make(map[string]int)

		allOk := true
		for _, table := range tables {
			var count int
			query := "SELECT COUNT(*) FROM [" + table + "]"
			if err := db.QueryRow(query).Scan(&count); err != nil {
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
