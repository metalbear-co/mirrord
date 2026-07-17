package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"
	"time"

	commonpb "go.temporal.io/api/common/v1"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/converter"
	"go.temporal.io/sdk/worker"
	"go.temporal.io/sdk/workflow"
)

const (
	workflowType   = "CheckoutWorkflow"
	activityType   = "ProcessOrder"
	userHeaderName = "x-user"
	keyHeaderName  = "mirrord-key"
)

type userCtxKey struct{}
type keyCtxKey struct{}

// userHeaderPropagator carries an "x-user" value from the workflow header onto
// the activities the workflow schedules. The mirrord operator reads it from
// each task for routing, so a header filter can keep both the workflow task and
// its activities on the same local worker.
type userHeaderPropagator struct{}

func (userHeaderPropagator) Inject(ctx context.Context, writer workflow.HeaderWriter) error {
	return writeUserHeader(ctx.Value(userCtxKey{}), writer)
}

func (userHeaderPropagator) InjectFromWorkflow(ctx workflow.Context, writer workflow.HeaderWriter) error {
	return writeUserHeader(ctx.Value(userCtxKey{}), writer)
}

func (userHeaderPropagator) Extract(ctx context.Context, reader workflow.HeaderReader) (context.Context, error) {
	if value := readHeader(reader, userHeaderName); value != "" {
		ctx = context.WithValue(ctx, userCtxKey{}, value)
	}
	if value := readHeader(reader, keyHeaderName); value != "" {
		ctx = context.WithValue(ctx, keyCtxKey{}, value)
	}
	return ctx, nil
}

func (userHeaderPropagator) ExtractToWorkflow(ctx workflow.Context, reader workflow.HeaderReader) (workflow.Context, error) {
	if value := readHeader(reader, userHeaderName); value != "" {
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

func readHeader(reader workflow.HeaderReader, name string) string {
	var value string
	_ = reader.ForEachKey(func(key string, payload *commonpb.Payload) error {
		if key == name {
			_ = converter.GetDefaultDataConverter().FromPayload(payload, &value)
		}
		return nil
	})
	return value
}

func CheckoutWorkflow(ctx workflow.Context, orderID string) (string, error) {
	info := workflow.GetInfo(ctx)
	workflowID := info.WorkflowExecution.ID

	ao := workflow.ActivityOptions{StartToCloseTimeout: time.Minute}
	ctx = workflow.WithActivityOptions(ctx, ao)

	var result string
	if err := workflow.ExecuteActivity(ctx, ProcessOrder, orderID).Get(ctx, &result); err != nil {
		return "", err
	}

	// Lines starting with "1:" are asserted by operator E2E tests (same convention as Pub/Sub).
	fmt.Printf("1:%s\n", workflowID)
	return result, nil
}

func ProcessOrder(ctx context.Context, orderID string) (string, error) {
	info := activity.GetInfo(ctx)
	log.Printf(
		"activity workflow_id=%s activity_type=%s order_id=%s",
		info.WorkflowExecution.ID,
		info.ActivityType.Name,
		orderID,
	)
	if key, ok := ctx.Value(keyCtxKey{}).(string); ok && key != "" {
		fmt.Printf("1:mirrord-key:%s\n", key)
	}
	return "processed:" + orderID, nil
}

func main() {
	address := envOr("TEMPORAL_ADDRESS", "localhost:7233")
	namespace := envOr("TEMPORAL_NAMESPACE", "default")
	taskQueue := envOr("TEMPORAL_TASK_QUEUE", "order-checkout")

	fmt.Fprintf(os.Stderr, "temporal-worker starting\n")
	fmt.Fprintf(os.Stderr, "  TEMPORAL_ADDRESS=%s\n", address)
	fmt.Fprintf(os.Stderr, "  TEMPORAL_NAMESPACE=%s\n", namespace)
	fmt.Fprintf(os.Stderr, "  TEMPORAL_TASK_QUEUE=%s\n", taskQueue)

	c, err := client.Dial(client.Options{
		HostPort:           address,
		Namespace:          namespace,
		ContextPropagators: []workflow.ContextPropagator{userHeaderPropagator{}},
	})
	if err != nil {
		log.Fatalf("failed to create Temporal client: %v", err)
	}
	defer c.Close()

	w := worker.New(c, taskQueue, worker.Options{})
	w.RegisterWorkflowWithOptions(CheckoutWorkflow, workflow.RegisterOptions{Name: workflowType})
	w.RegisterActivityWithOptions(ProcessOrder, activity.RegisterOptions{Name: activityType})

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	errCh := make(chan error, 1)
	go func() {
		errCh <- w.Run(worker.InterruptCh())
	}()

	select {
	case <-ctx.Done():
		w.Stop()
		<-errCh
	case err := <-errCh:
		if err != nil {
			log.Fatalf("worker failed: %v", err)
		}
	}
}

func envOr(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}
