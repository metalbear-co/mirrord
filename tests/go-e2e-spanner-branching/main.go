// Verification app for Spanner DB branching e2e tests.
//
// mirrord overrides SPANNER_EMULATOR_HOST to point at the branch emulator; the official Spanner
// client detects that variable on its own, so this connects to the branch with no special wiring.
// It prints the branch table set and per-table row counts on stable lines the Rust test waits for.
package main

import (
	"context"
	"fmt"
	"log"
	"net/http"
	"os"
	"time"

	"cloud.google.com/go/spanner"
	"google.golang.org/api/iterator"
)

func env(name string) string {
	v := os.Getenv(name)
	if v == "" {
		log.Fatalf("%s not set", name)
	}
	return v
}

func main() {
	log.SetFlags(log.LstdFlags)
	log.Println("Spanner test app starting")

	log.Printf("SPANNER_EMULATOR_HOST: %s", os.Getenv("SPANNER_EMULATOR_HOST"))

	dbPath := fmt.Sprintf(
		"projects/%s/instances/%s/databases/%s",
		env("SPANNER_PROJECT_ID"),
		env("SPANNER_INSTANCE_ID"),
		env("SPANNER_DATABASE_ID"),
	)

	ctx := context.Background()

	var client *spanner.Client
	var err error
	for attempt := 1; attempt <= 30; attempt++ {
		client, err = spanner.NewClient(ctx, dbPath)
		if err == nil {
			break
		}
		log.Printf("Waiting for branch Spanner (attempt %d/30): %v", attempt, err)
		time.Sleep(2 * time.Second)
	}
	if err != nil {
		log.Fatalf("Could not connect to branch Spanner: %v", err)
	}
	defer client.Close()
	log.Println("Connected to branch Spanner")

	tables := listTables(ctx, client)
	log.Printf("Tables: %d", len(tables))
	for _, t := range tables {
		log.Printf("Table %s: %d rows", t, countRows(ctx, client, t))
	}

	log.Println("Verification complete")

	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("healthy"))
	})
	if err := http.ListenAndServe(":8080", nil); err != nil {
		log.Printf("health server stopped: %v", err)
	}
}

// listTables returns the branch's base tables. Retries so a branch that is still copying schema is
// given time to catch up before we report the set.
func listTables(ctx context.Context, client *spanner.Client) []string {
	const sql = "SELECT table_name FROM information_schema.tables " +
		"WHERE table_schema = '' AND table_type = 'BASE TABLE' ORDER BY table_name"

	var tables []string
	for attempt := 1; attempt <= 15; attempt++ {
		tables = nil
		iter := client.Single().Query(ctx, spanner.Statement{SQL: sql})
		err := func() error {
			defer iter.Stop()
			for {
				row, err := iter.Next()
				if err == iterator.Done {
					return nil
				}
				if err != nil {
					return err
				}
				var name string
				if err := row.Columns(&name); err != nil {
					return err
				}
				tables = append(tables, name)
			}
		}()
		if err == nil && len(tables) > 0 {
			return tables
		}
		if err != nil {
			log.Printf("Waiting for branch schema (attempt %d/15): %v", attempt, err)
		}
		time.Sleep(1 * time.Second)
	}
	return tables
}

func countRows(ctx context.Context, client *spanner.Client, table string) int64 {
	sql := fmt.Sprintf("SELECT COUNT(*) FROM `%s`", table)
	iter := client.Single().Query(ctx, spanner.Statement{SQL: sql})
	defer iter.Stop()
	row, err := iter.Next()
	if err != nil {
		log.Printf("count query for %s failed: %v", table, err)
		return -1
	}
	var count int64
	if err := row.Columns(&count); err != nil {
		log.Printf("reading count for %s failed: %v", table, err)
		return -1
	}
	return count
}
