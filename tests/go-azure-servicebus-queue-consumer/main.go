package main

import (
	"context"
	"fmt"
	"log"
	"net"
	"os"
	"os/signal"
	"sort"
	"syscall"
	"time"

	"github.com/Azure/azure-sdk-for-go/sdk/messaging/azservicebus"
)

// SERVICEBUS_TEST_MODE is a free-form label the test sets so its scenario shows
// up in the consumer logs. The consumer itself only needs to know whether to
// read from a queue or from a topic/subscription, and it works that out from the
// env vars the test put on the pod rather than from the label. So a new test
// does not need a change here as long as it sets the usual env vars: a topic
// scenario sets SERVICEBUS_TOPIC_NAME and SERVICEBUS_SUBSCRIPTION_NAME, every
// other (queue) scenario sets SERVICEBUS_QUEUE_NAME.
type testMode struct {
	label            string
	queueName        string
	topicName        string
	subscriptionName string
}

func modeFromEnv() testMode {
	label := os.Getenv("SERVICEBUS_TEST_MODE")
	if label == "" {
		log.Fatal("SERVICEBUS_TEST_MODE must be set")
	}

	if os.Getenv("SERVICEBUS_TOPIC_NAME") != "" || os.Getenv("SERVICEBUS_SUBSCRIPTION_NAME") != "" {
		return testMode{
			label:            label + " (topic/subscription)",
			topicName:        requireEnv("SERVICEBUS_TOPIC_NAME"),
			subscriptionName: requireEnv("SERVICEBUS_SUBSCRIPTION_NAME"),
		}
	}

	return testMode{
		label:     label + " (queue)",
		queueName: requireEnv("SERVICEBUS_QUEUE_NAME"),
	}
}

func (m testMode) isTopicMode() bool {
	return m.topicName != "" && m.subscriptionName != ""
}

func (m testMode) createReceiver(client *azservicebus.Client) (*azservicebus.Receiver, error) {
	if m.isTopicMode() {
		return client.NewReceiverForSubscription(m.topicName, m.subscriptionName, nil)
	}
	return client.NewReceiverForQueue(m.queueName, nil)
}

func requireEnv(key string) string {
	val := os.Getenv(key)
	if val == "" {
		log.Fatalf("%s must be set", key)
	}
	return val
}

func main() {
	connStr := requireEnv("SERVICEBUS_CONNECTION_STRING")
	_, printProps := os.LookupEnv("SERVICEBUS_TEST_PRINT_PROPS")

	mode := modeFromEnv()
	fmt.Fprintf(os.Stderr, "go-azure-servicebus-consumer starting (%s)\n", mode.label)
	if mode.queueName != "" {
		fmt.Fprintf(os.Stderr, "  queue=%s\n", mode.queueName)
	}
	if mode.topicName != "" {
		fmt.Fprintf(os.Stderr, "  topic=%s subscription=%s\n", mode.topicName, mode.subscriptionName)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	stderr("Creating AMQP client (lazy, no network yet)...")
	client, err := azservicebus.NewClientFromConnectionString(connStr, nil)
	if err != nil {
		log.Fatalf("Failed to create Service Bus client: %v", err)
	}
	defer client.Close(ctx)
	stderr("Client created")

	stderr("Creating receiver (lazy, no network yet)...")
	receiver, err := mode.createReceiver(client)
	if err != nil {
		log.Fatalf("Failed to create receiver: %v", err)
	}
	defer receiver.Close(ctx)
	stderr("Receiver created")

	// Raw TCP probe: verify we can reach the emulator before the SDK
	// tries its own connection. This tells us whether the network path
	// through mirrord works at all.
	if ep := extractHost(connStr); ep != "" {
		stderr("TCP probe to %s ...", ep)
		probeConn, probeErr := net.DialTimeout("tcp", ep, 10*time.Second)
		if probeErr != nil {
			stderr("TCP probe FAILED: %v", probeErr)
		} else {
			stderr("TCP probe OK (local=%s remote=%s)", probeConn.LocalAddr(), probeConn.RemoteAddr())
			probeConn.Close()
		}
	}

	fmt.Println("READY")

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigChan
		stderr("Received shutdown signal")
		cancel()
	}()

	// Periodic heartbeat so we can tell the process is alive even when
	// ReceiveMessages blocks for a long time.
	go func() {
		tick := time.NewTicker(10 * time.Second)
		defer tick.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case t := <-tick.C:
				stderr("heartbeat: still alive at %s", t.Format(time.RFC3339))
			}
		}
	}()

	stderr("Starting receive loop")
	iteration := 0
	for {
		if ctx.Err() != nil {
			break
		}
		iteration++

		stderr("[iter %d] Calling ReceiveMessages (max=10, timeout=5s)...", iteration)
		start := time.Now()
		messages, err := receiver.ReceiveMessages(ctx, 10, &azservicebus.ReceiveMessagesOptions{
			TimeAfterFirstMessage: 5 * time.Second,
		})
		elapsed := time.Since(start)

		if err != nil {
			if ctx.Err() != nil {
				stderr("[iter %d] Context cancelled, exiting", iteration)
				break
			}
			stderr("[iter %d] Receive error after %s: %v", iteration, elapsed, err)
			time.Sleep(time.Second)
			continue
		}

		stderr("[iter %d] ReceiveMessages returned %d messages after %s", iteration, len(messages), elapsed)
		for i, msg := range messages {
			body := string(msg.Body)
			stderr("[iter %d] msg[%d]: id=%s body_len=%d body=%q", iteration, i, msg.MessageID, len(msg.Body), body)
			fmt.Printf("1:%s\n", body)
			if printProps && msg.ApplicationProperties != nil {
				for _, key := range sortedKeys(msg.ApplicationProperties) {
					fmt.Printf("1:prop:%s=%v\n", key, msg.ApplicationProperties[key])
				}
			}
			if err := receiver.CompleteMessage(ctx, msg, nil); err != nil {
				stderr("[iter %d] Failed to complete message %s: %v", iteration, msg.MessageID, err)
			}
		}
	}
}

func stderr(format string, args ...any) {
	msg := fmt.Sprintf(format, args...)
	fmt.Fprintf(os.Stderr, "[consumer %s] %s\n", time.Now().Format("15:04:05.000"), msg)
}

// extractHost pulls "host:port" from an AMQP connection string so we can do
// a raw TCP probe independently of the SDK.
func extractHost(connStr string) string {
	for _, part := range splitSemicolon(connStr) {
		if len(part) > 9 && part[:9] == "Endpoint=" {
			ep := part[9:]
			// strip scheme (sb:// or amqp://)
			for i := 0; i < len(ep); i++ {
				if ep[i] == '/' && i+1 < len(ep) && ep[i+1] == '/' {
					ep = ep[i+2:]
					break
				}
			}
			// strip trailing slash
			if len(ep) > 0 && ep[len(ep)-1] == '/' {
				ep = ep[:len(ep)-1]
			}
			// add default AMQP port if none
			if _, _, err := net.SplitHostPort(ep); err != nil {
				ep = ep + ":5672"
			}
			return ep
		}
	}
	return ""
}

func splitSemicolon(s string) []string {
	var parts []string
	start := 0
	for i := 0; i <= len(s); i++ {
		if i == len(s) || s[i] == ';' {
			if i > start {
				parts = append(parts, s[start:i])
			}
			start = i + 1
		}
	}
	return parts
}

func sortedKeys(m map[string]any) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}
