// E2E test app for GENERIC db branching (RFC 0008). Runs locally under `mirrord exec`
// and prints assertion lines the Rust e2e tests wait for.
//
// Two modes, selected by which env vars are present:
//
// Valkey mode (VALKEY_ADDR set): proves args-based bootstrap ($(MIRRORD_PARAM_PASSWORD)),
// composite host:port rewriting, and write isolation. The untouched VALKEY_SOURCE_ADDR
// twin lets it dial the source directly for the isolation check.
//
// InfluxDB mode (INFLUXDB_URL set): proves env-based bootstrap (DOCKER_INFLUXDB_INIT_*),
// URL-shaped var rewriting, the direct-Secret token param, and isolation via per-instance
// org IDs.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/redis/go-redis/v9"
)

func main() {
	log.SetFlags(log.LstdFlags)
	log.Println("Generic test app starting")

	if os.Getenv("INFLUXDB_URL") != "" {
		influxMode()
	} else {
		valkeyMode()
	}

	log.Println("Verification complete")
}

func valkeyMode() {
	addr := os.Getenv("VALKEY_ADDR")
	sourceAddr := os.Getenv("VALKEY_SOURCE_ADDR")
	password := os.Getenv("VALKEY_PASSWORD")

	log.Printf("VALKEY_ADDR: %s", addr)
	log.Printf("VALKEY_SOURCE_ADDR: %s", sourceAddr)
	log.Printf("REDIRECTED: %v", addr != sourceAddr && addr != "")

	ctx := context.Background()
	branch := redis.NewClient(&redis.Options{Addr: addr, Password: password})
	defer branch.Close()

	if err := pingWithRetry(ctx, branch); err != nil {
		log.Fatalf("branch ping failed: %v", err)
	}
	// Authenticated PONG = the branch was bootstrapped with the app's real password
	// via $(MIRRORD_PARAM_PASSWORD) in the branch args.
	log.Println("PING: PONG")

	size, err := branch.DBSize(ctx).Result()
	if err != nil {
		log.Fatalf("branch DBSIZE failed: %v", err)
	}
	log.Printf("DBSIZE: %d", size)

	if err := branch.Set(ctx, "branch-canary", "written-under-mirrord", 0).Err(); err != nil {
		log.Fatalf("branch SET failed: %v", err)
	}
	val, err := branch.Get(ctx, "branch-canary").Result()
	if err != nil || val != "written-under-mirrord" {
		log.Fatalf("branch GET failed: val=%q err=%v", val, err)
	}
	log.Println("CANARY WRITTEN: ok")

	// Dial the SOURCE directly (mirrord's outgoing proxy resolves the service name) and
	// prove the canary never reached it.
	source := redis.NewClient(&redis.Options{Addr: sourceAddr, Password: password})
	defer source.Close()
	if err := pingWithRetry(ctx, source); err != nil {
		log.Fatalf("source ping failed: %v", err)
	}
	sourceSize, err := source.DBSize(ctx).Result()
	if err != nil {
		log.Fatalf("source DBSIZE failed: %v", err)
	}
	log.Printf("SOURCE DBSIZE: %d", sourceSize)

	exists, err := source.Exists(ctx, "branch-canary").Result()
	if err != nil {
		log.Fatalf("source EXISTS failed: %v", err)
	}
	log.Printf("SOURCE HAS CANARY: %v", exists > 0)
}

func pingWithRetry(ctx context.Context, client *redis.Client) error {
	var err error
	for attempt := 1; attempt <= 30; attempt++ {
		pingCtx, cancel := context.WithTimeout(ctx, 5*time.Second)
		err = client.Ping(pingCtx).Err()
		cancel()
		if err == nil {
			return nil
		}
		time.Sleep(2 * time.Second)
	}
	return err
}

func influxMode() {
	baseURL := os.Getenv("INFLUXDB_URL")
	sourceURL := os.Getenv("INFLUXDB_SOURCE_URL")
	token := os.Getenv("INFLUXDB_TOKEN")
	org := os.Getenv("INFLUXDB_ORG")
	bucket := os.Getenv("INFLUXDB_BUCKET")

	log.Printf("INFLUXDB_URL: %s", baseURL)
	log.Printf("INFLUXDB_SOURCE_URL: %s", sourceURL)
	log.Printf("REDIRECTED: %v", baseURL != sourceURL && baseURL != "")

	client := &http.Client{Timeout: 10 * time.Second}

	if err := waitHealthy(client, baseURL); err != nil {
		log.Fatalf("branch health failed: %v", err)
	}
	log.Println("HEALTH: pass")

	// The org is created per instance during setup, so branch and source both have an org
	// with this name but DIFFERENT ids - a per-instance identity check.
	branchOrg := orgID(client, baseURL, token, org)
	sourceOrg := orgID(client, sourceURL, token, org)
	log.Printf("ORG IDS DIFFER: %v", branchOrg != "" && sourceOrg != "" && branchOrg != sourceOrg)

	// Write a point through the branch, with the app's own (untouched) token.
	line := "generic_e2e,writer=local value=7"
	if err := writePoint(client, baseURL, token, org, bucket, line); err != nil {
		log.Fatalf("branch write failed: %v", err)
	}
	log.Println("POINT WRITTEN: ok")

	branchData, err := queryRange(client, baseURL, token, org, bucket)
	if err != nil {
		log.Fatalf("branch query failed: %v", err)
	}
	log.Printf("BRANCH HAS POINT: %v", strings.Contains(branchData, "generic_e2e"))

	sourceData, err := queryRange(client, sourceURL, token, org, bucket)
	if err != nil {
		log.Fatalf("source query failed: %v", err)
	}
	log.Printf("SOURCE HAS POINT: %v", strings.Contains(sourceData, "generic_e2e"))
}

func waitHealthy(client *http.Client, base string) error {
	var lastErr error
	for attempt := 1; attempt <= 30; attempt++ {
		resp, err := client.Get(base + "/health")
		if err == nil {
			resp.Body.Close()
			if resp.StatusCode == http.StatusOK {
				return nil
			}
			lastErr = fmt.Errorf("health returned %d", resp.StatusCode)
		} else {
			lastErr = err
		}
		time.Sleep(2 * time.Second)
	}
	return lastErr
}

func orgID(client *http.Client, base, token, org string) string {
	req, _ := http.NewRequest(http.MethodGet, fmt.Sprintf("%s/api/v2/orgs?org=%s", base, org), nil)
	req.Header.Set("Authorization", "Token "+token)
	resp, err := client.Do(req)
	if err != nil {
		log.Printf("orgID(%s) failed: %v", base, err)
		return ""
	}
	defer resp.Body.Close()
	var parsed struct {
		Orgs []struct {
			ID string `json:"id"`
		} `json:"orgs"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&parsed); err != nil || len(parsed.Orgs) == 0 {
		return ""
	}
	return parsed.Orgs[0].ID
}

func writePoint(client *http.Client, base, token, org, bucket, line string) error {
	url := fmt.Sprintf("%s/api/v2/write?org=%s&bucket=%s&precision=s", base, org, bucket)
	req, _ := http.NewRequest(http.MethodPost, url, strings.NewReader(line))
	req.Header.Set("Authorization", "Token "+token)
	resp, err := client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	if resp.StatusCode >= 300 {
		body, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("write returned %d: %s", resp.StatusCode, body)
	}
	return nil
}

func queryRange(client *http.Client, base, token, org, bucket string) (string, error) {
	flux := fmt.Sprintf(`from(bucket:"%s") |> range(start:-1h)`, bucket)
	url := fmt.Sprintf("%s/api/v2/query?org=%s", base, org)
	req, _ := http.NewRequest(http.MethodPost, url, strings.NewReader(flux))
	req.Header.Set("Authorization", "Token "+token)
	req.Header.Set("Content-Type", "application/vnd.flux")
	resp, err := client.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return "", err
	}
	if resp.StatusCode >= 300 {
		return "", fmt.Errorf("query returned %d: %s", resp.StatusCode, body)
	}
	return string(body), nil
}
