package main

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"slices"
	"strings"
	"syscall"
	"time"

	"github.com/aws/aws-sdk-go-v2/aws"
	"github.com/aws/aws-sdk-go-v2/config"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb"
	"github.com/aws/aws-sdk-go-v2/service/dynamodb/types"
)

func init() {
	// Ensure output is not buffered
	log.SetFlags(log.LstdFlags)
}

func main() {
	// Print immediately to stderr
	fmt.Fprintln(os.Stderr, "DynamoDB test app starting")
	log.Println("DynamoDB test app starting")

	// The AWS SDK resolves AWS_ENDPOINT_URL_DYNAMODB / AWS_ENDPOINT_URL automatically via
	// config.LoadDefaultConfig -- logged here so e2e tests can see what the operator's session
	// override actually set, the same way the other dialects log their connection env var.
	log.Printf("AWS_ENDPOINT_URL_DYNAMODB: %s", os.Getenv("AWS_ENDPOINT_URL_DYNAMODB"))
	log.Printf("AWS_ENDPOINT_URL: %s", os.Getenv("AWS_ENDPOINT_URL"))
	log.Printf("AWS_REGION: %s", os.Getenv("AWS_REGION"))

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	cfg, err := config.LoadDefaultConfig(ctx)
	if err != nil {
		log.Fatalf("Failed to load AWS config: %v", err)
	}

	client := dynamodb.NewFromConfig(cfg)

	// Retry (branch dynamodb-local may still be starting up).
	var tables []string
	for attempt := 1; attempt <= 15; attempt++ {
		listCtx, listCancel := context.WithTimeout(context.Background(), 5*time.Second)
		out, listErr := client.ListTables(listCtx, &dynamodb.ListTablesInput{})
		listCancel()
		err = listErr
		if err == nil {
			tables = out.TableNames
			break
		}
		log.Printf("Waiting for DB (attempt %d/15): %v", attempt, err)
		time.Sleep(2 * time.Second)
	}
	if err != nil {
		log.Fatalf("DB connection failed: %v", err)
	}

	log.Println("Connected to database")

	// Wait for data to appear (the branch-init sidecar may still be copying tables).
	maxWaitAttempts := 30
	for attempt := 1; attempt <= maxWaitAttempts; attempt++ {
		listCtx, listCancel := context.WithTimeout(context.Background(), 5*time.Second)
		out, listErr := client.ListTables(listCtx, &dynamodb.ListTablesInput{})
		listCancel()
		if listErr == nil && len(out.TableNames) > 0 {
			tables = out.TableNames
			log.Printf("Tables found after %d attempts", attempt)
			break
		}
		if attempt == maxWaitAttempts {
			log.Printf("Warning: no tables found after %d attempts, proceeding anyway", maxWaitAttempts)
		} else {
			count := 0
			if listErr == nil {
				count = len(out.TableNames)
			}
			log.Printf("Waiting for tables (attempt %d/%d): count=%d", attempt, maxWaitAttempts, count)
			time.Sleep(1 * time.Second)
		}
	}

	// EXPECTED_TABLES (comma-separated) names tables the e2e harness's fixture seeds, even ones
	// a table filter is expected to exclude entirely from the branch. Without this, an excluded
	// table would never appear in ListTables and a test could never assert its absence -- only
	// ever assert which tables *are* present, which can't distinguish "filter works" from
	// "filter is silently ignored". Reporting 0 for those (without calling Scan on a table that
	// doesn't exist) closes that gap. Tables outside this list are still reported normally, so
	// the app works the same with no fixture knowledge at all when the env var is unset.
	reportTables := slices.Clone(tables)
	for _, expected := range splitNonEmpty(os.Getenv("EXPECTED_TABLES"), ",") {
		if !slices.Contains(reportTables, expected) {
			reportTables = append(reportTables, expected)
		}
	}

	// Only scan tables that actually exist; counts[table] is 0 for the rest (Go's zero value
	// for a missing map key), which is exactly what "filtered out of the branch" should report.
	counts := scanTableCounts(ctx, client, tables)
	for _, table := range reportTables {
		log.Printf("Table %s: %d items", table, counts[table])
	}

	log.Println("Database verification complete")

	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		healthCtx, healthCancel := context.WithTimeout(r.Context(), 5*time.Second)
		defer healthCancel()

		out, err := client.ListTables(healthCtx, &dynamodb.ListTablesInput{})

		result := map[string]interface{}{}
		if err != nil {
			result["status"] = "unhealthy"
			result["error"] = err.Error()
			w.WriteHeader(http.StatusInternalServerError)
		} else {
			result["status"] = "healthy"
			result["tables"] = scanTableCounts(healthCtx, client, out.TableNames)
			w.WriteHeader(http.StatusOK)
		}

		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(result)
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

// splitNonEmpty splits s on sep and drops empty/whitespace-only fields, so an unset or
// trailing-comma env var yields an empty slice rather than a slice with one blank entry.
func splitNonEmpty(s string, sep string) []string {
	var result []string
	for _, part := range strings.Split(s, sep) {
		if trimmed := strings.TrimSpace(part); trimmed != "" {
			result = append(result, trimmed)
		}
	}
	return result
}

// scanTableCounts returns each table's item count via a count-only Scan, paginating until
// every page has been consumed.
func scanTableCounts(ctx context.Context, client *dynamodb.Client, tables []string) map[string]int64 {
	counts := make(map[string]int64, len(tables))

	for _, table := range tables {
		var total int64
		var exclusiveStartKey map[string]types.AttributeValue

		for {
			scanCtx, scanCancel := context.WithTimeout(ctx, 5*time.Second)
			out, err := client.Scan(scanCtx, &dynamodb.ScanInput{
				TableName:         aws.String(table),
				Select:            types.SelectCount,
				ExclusiveStartKey: exclusiveStartKey,
			})
			scanCancel()
			if err != nil {
				log.Printf("Table %s: scan error (%v)", table, err)
				break
			}

			total += int64(out.Count)
			exclusiveStartKey = out.LastEvaluatedKey
			if len(exclusiveStartKey) == 0 {
				break
			}
		}

		counts[table] = total
	}

	return counts
}
