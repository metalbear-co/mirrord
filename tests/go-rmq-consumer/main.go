package main

import (
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"
	"time"

	amqp "github.com/rabbitmq/amqp091-go"
)

// How long to wait between attempts to dial RabbitMQ again after the connection drops.
const reconnectDelay = time.Second

type queue struct {
	name string
	num  int
}

// startConsumers opens one channel per queue on conn and prints every delivery in the background.
// The printing goroutines end on their own when the connection closes.
func startConsumers(conn *amqp.Connection, queues []queue, printHeaders bool) error {
	for _, q := range queues {
		ch, err := conn.Channel()
		if err != nil {
			return fmt.Errorf("failed to open channel for queue %s: %w", q.name, err)
		}

		msgs, err := ch.Consume(
			q.name,
			"",    // consumer tag
			true,  // auto-ack
			false, // exclusive
			false, // no-local
			false, // no-wait
			nil,   // args
		)
		if err != nil {
			return fmt.Errorf("failed to start consuming from queue %s: %w", q.name, err)
		}

		fmt.Fprintf(os.Stderr, "Consuming from queue %s (%d)\n", q.name, q.num)
		go printDeliveries(msgs, q.num, printHeaders)
	}
	return nil
}

func printDeliveries(msgs <-chan amqp.Delivery, queueNum int, printHeaders bool) {
	for msg := range msgs {
		fmt.Printf("%d:%s\n", queueNum, string(msg.Body))
		if printHeaders {
			for key, val := range msg.Headers {
				fmt.Printf("%d:header:%s=%v\n", queueNum, key, val)
			}
		}
	}
}

// keepConsuming dials RabbitMQ again and restarts the consumers each time the connection closes.
//
// When the mirrord operator restarts, the session reconnects and every TCP connection the app had
// through it is closed. Without this the delivery channels close, the consumers stop, and the app
// keeps running without printing anything, so tests that publish after the restart time out.
func keepConsuming(conn *amqp.Connection, amqpURL string, queues []queue, printHeaders bool) {
	for {
		reason := <-conn.NotifyClose(make(chan *amqp.Error, 1))
		fmt.Fprintf(os.Stderr, "RabbitMQ connection closed (%v), reconnecting\n", reason)
		conn = reconnect(amqpURL, queues, printHeaders)
	}
}

// reconnect retries until it has a new connection with every consumer running on it.
func reconnect(amqpURL string, queues []queue, printHeaders bool) *amqp.Connection {
	for {
		time.Sleep(reconnectDelay)

		conn, err := amqp.Dial(amqpURL)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Failed to reconnect to RabbitMQ, retrying: %v\n", err)
			continue
		}

		if err := startConsumers(conn, queues, printHeaders); err != nil {
			fmt.Fprintf(os.Stderr, "Failed to restart consumers after reconnecting, retrying: %v\n", err)
			conn.Close()
			continue
		}

		fmt.Fprintf(os.Stderr, "Reconnected to RabbitMQ at %s\n", amqpURL)
		return conn
	}
}

func main() {
	amqpURL := os.Getenv("RABBIT_MQ_URL")
	if amqpURL == "" {
		amqpURL = "amqp://guest:guest@localhost:5672/"
	}

	q1Name := os.Getenv("RABBIT_MQ_INVENTORY_QUEUE")
	q2Name := os.Getenv("RABBIT_MQ_ORDERS_QUEUE")

	if q1Name == "" {
		log.Fatal("RABBIT_MQ_INVENTORY_QUEUE must be set")
	}

	_, printHeaders := os.LookupEnv("RMQ_TEST_PRINT_HEADERS")

	queues := []queue{{name: q1Name, num: 1}}
	if q2Name != "" {
		queues = append(queues, queue{name: q2Name, num: 2})
	}

	// The first connection fails fast so a wrong URL or a missing queue shows up as a startup error
	// instead of an endless retry loop.
	conn, err := amqp.Dial(amqpURL)
	if err != nil {
		log.Fatalf("Failed to connect to RabbitMQ at %s: %v", amqpURL, err)
	}

	fmt.Fprintf(os.Stderr, "Connected to RabbitMQ at %s\n", amqpURL)

	if err := startConsumers(conn, queues, printHeaders); err != nil {
		log.Fatal(err)
	}

	go keepConsuming(conn, amqpURL, queues, printHeaders)

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)

	<-sigChan
	fmt.Fprintln(os.Stderr, "Received shutdown signal")
}
