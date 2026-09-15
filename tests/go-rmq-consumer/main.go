package main

import (
	"fmt"
	"log"
	"os"
	"os/signal"
	"sync"
	"syscall"

	amqp "github.com/rabbitmq/amqp091-go"
)

func consumeQueue(conn *amqp.Connection, queueName string, queueNum int, printHeaders bool, wg *sync.WaitGroup) {
	defer wg.Done()

	ch, err := conn.Channel()
	if err != nil {
		log.Fatalf("Failed to open channel for queue %s: %v", queueName, err)
	}
	defer ch.Close()

	msgs, err := ch.Consume(
		queueName,
		"",    // consumer tag
		true,  // auto-ack
		false, // exclusive
		false, // no-local
		false, // no-wait
		nil,   // args
	)
	if err != nil {
		log.Fatalf("Failed to start consuming from queue %s: %v", queueName, err)
	}

	fmt.Fprintf(os.Stderr, "Consuming from queue %s (%d)\n", queueName, queueNum)

	for msg := range msgs {
		fmt.Printf("%d:%s\n", queueNum, string(msg.Body))
		if printHeaders {
			for key, val := range msg.Headers {
				fmt.Printf("%d:header:%s=%v\n", queueNum, key, val)
			}
		}
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

	conn, err := amqp.Dial(amqpURL)
	if err != nil {
		log.Fatalf("Failed to connect to RabbitMQ at %s: %v", amqpURL, err)
	}
	defer conn.Close()

	fmt.Fprintf(os.Stderr, "Connected to RabbitMQ at %s\n", amqpURL)

	var wg sync.WaitGroup

	wg.Add(1)
	go consumeQueue(conn, q1Name, 1, printHeaders, &wg)

	if q2Name != "" {
		wg.Add(1)
		go consumeQueue(conn, q2Name, 2, printHeaders, &wg)
	}

	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, syscall.SIGINT, syscall.SIGTERM)

	<-sigChan
	fmt.Fprintln(os.Stderr, "Received shutdown signal")
}
