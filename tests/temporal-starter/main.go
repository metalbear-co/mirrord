package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"strings"
	"time"

	commonpb "go.temporal.io/api/common/v1"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/converter"
	"go.temporal.io/sdk/workflow"
)

const (
	workflowType   = "CheckoutWorkflow"
	userHeaderName = "x-user"
)

type userCtxKey struct{}

// userHeaderPropagator copies an "x-user" value between the Go context and the
// Temporal header. On workflow start the value is written into the header, so it
// lands in the WorkflowExecutionStarted event where the mirrord operator reads
// it for routing.
type userHeaderPropagator struct{}

func (userHeaderPropagator) Inject(ctx context.Context, writer workflow.HeaderWriter) error {
	return writeUserHeader(ctx.Value(userCtxKey{}), writer)
}

func (userHeaderPropagator) InjectFromWorkflow(ctx workflow.Context, writer workflow.HeaderWriter) error {
	return writeUserHeader(ctx.Value(userCtxKey{}), writer)
}

func (userHeaderPropagator) Extract(ctx context.Context, reader workflow.HeaderReader) (context.Context, error) {
	if value := readUserHeader(reader); value != "" {
		ctx = context.WithValue(ctx, userCtxKey{}, value)
	}
	return ctx, nil
}

func (userHeaderPropagator) ExtractToWorkflow(ctx workflow.Context, reader workflow.HeaderReader) (workflow.Context, error) {
	if value := readUserHeader(reader); value != "" {
		ctx = workflow.WithValue(ctx, userCtxKey{}, value)
	}
	return ctx, nil
}

func writeUserHeader(raw interface{}, writer workflow.HeaderWriter) error {
	value, ok := raw.(string)
	if !ok || value == "" {
		return nil
	}
	payload, err := converter.GetDefaultDataConverter().ToPayload(value)
	if err != nil {
		return err
	}
	writer.Set(userHeaderName, payload)
	return nil
}

func readUserHeader(reader workflow.HeaderReader) string {
	var value string
	_ = reader.ForEachKey(func(key string, payload *commonpb.Payload) error {
		if key == userHeaderName {
			_ = converter.GetDefaultDataConverter().FromPayload(payload, &value)
		}
		return nil
	})
	return value
}

func main() {
	address := envOr("TEMPORAL_ADDRESS", "localhost:7233")
	namespace := envOr("TEMPORAL_NAMESPACE", "default")
	taskQueue := envOr("TEMPORAL_TASK_QUEUE", "order-checkout")
	headerUser := os.Getenv("WORKFLOW_HEADER_USER")

	workflowIDs := os.Getenv("WORKFLOW_IDS")
	if workflowIDs == "" && len(os.Args) > 1 {
		workflowIDs = strings.Join(os.Args[1:], ",")
	}
	if workflowIDs == "" {
		log.Fatal("WORKFLOW_IDS env var or command-line args are required")
	}

	// We reach the frontend through a kube port-forward, which sets up the pod
	// stream lazily on the first connection. gRPC dials eagerly and reads the
	// HTTP/2 server preface, so the first attempt can race the port-forward and
	// fail with "error reading server preface: EOF". Retry for a short while so
	// a transient port-forward hiccup does not fail the whole test.
	c, err := dialWithRetry(client.Options{
		HostPort:           address,
		Namespace:          namespace,
		ContextPropagators: []workflow.ContextPropagator{userHeaderPropagator{}},
	}, 30*time.Second)
	if err != nil {
		log.Fatalf("failed to create Temporal client: %v", err)
	}
	defer c.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	if headerUser != "" {
		ctx = context.WithValue(ctx, userCtxKey{}, headerUser)
	}

	for _, workflowID := range strings.Split(workflowIDs, ",") {
		workflowID = strings.TrimSpace(workflowID)
		if workflowID == "" {
			continue
		}

		run, err := c.ExecuteWorkflow(ctx, client.StartWorkflowOptions{
			ID:        workflowID,
			TaskQueue: taskQueue,
		}, workflowType, workflowID)
		if err != nil {
			log.Fatalf("failed to start workflow %s: %v", workflowID, err)
		}

		fmt.Fprintf(os.Stderr, "started workflow %s (run_id=%s)\n", workflowID, run.GetRunID())
	}
}

func dialWithRetry(options client.Options, timeout time.Duration) (client.Client, error) {
	deadline := time.Now().Add(timeout)
	var lastErr error
	for {
		c, err := client.Dial(options)
		if err == nil {
			return c, nil
		}
		lastErr = err
		if time.Now().After(deadline) {
			return nil, lastErr
		}
		log.Printf("Temporal dial failed, retrying: %v", err)
		time.Sleep(time.Second)
	}
}

func envOr(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}
