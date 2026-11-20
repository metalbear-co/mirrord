// Sends one message to an Azure Service Bus queue, for driving splitting tests
// from inside the cluster: Service Bus has no producer CLI, so this ships in
// the apps image and runs via `kubectl exec` in the consumer pod, inheriting
// its connection env the way temporal-starter does.
//
// SERVICEBUS_FULLY_QUALIFIED_NAMESPACE (signing in as the pod's Entra identity) or
// SERVICEBUS_CONNECTION_STRING, and SERVICEBUS_QUEUE_NAME, come from the pod.
// SEND_BODY is the message body. SEND_USER, when set, is stamped as the
// `mirrord-user` application property the split filters match on; leave it
// unset to send a message with no routing property at all.
package main

import (
	"context"
	"fmt"
	"log"
	"os"
	"time"

	"github.com/Azure/azure-sdk-for-go/sdk/azidentity"
	"github.com/Azure/azure-sdk-for-go/sdk/messaging/azservicebus"
)

// The same two ways in as the consumer: a namespace host with the ambient Azure credential
// (workload identity in a pod, `az login` on a machine), else the emulator's connection string.
func newClient() *azservicebus.Client {
	if fqns := os.Getenv("SERVICEBUS_FULLY_QUALIFIED_NAMESPACE"); fqns != "" {
		cred, err := azidentity.NewDefaultAzureCredential(nil)
		if err != nil {
			log.Fatalf("failed to build the Azure credential: %v", err)
		}
		client, err := azservicebus.NewClient(fqns, cred, nil)
		if err != nil {
			log.Fatalf("failed to create client for %s: %v", fqns, err)
		}
		return client
	}
	connectionString := os.Getenv("SERVICEBUS_CONNECTION_STRING")
	if connectionString == "" {
		log.Fatal("SERVICEBUS_FULLY_QUALIFIED_NAMESPACE or SERVICEBUS_CONNECTION_STRING must be set")
	}
	client, err := azservicebus.NewClientFromConnectionString(connectionString, nil)
	if err != nil {
		log.Fatalf("failed to create client: %v", err)
	}
	return client
}

func main() {
	queueName := os.Getenv("SERVICEBUS_QUEUE_NAME")
	if queueName == "" {
		log.Fatal("SERVICEBUS_QUEUE_NAME must be set")
	}
	body := os.Getenv("SEND_BODY")
	if body == "" {
		log.Fatal("SEND_BODY must be set")
	}

	sender, err := newClient().NewSender(queueName, nil)
	if err != nil {
		log.Fatalf("failed to create sender: %v", err)
	}

	message := &azservicebus.Message{Body: []byte(body)}
	if user := os.Getenv("SEND_USER"); user != "" {
		message.ApplicationProperties = map[string]any{"mirrord-user": user}
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	if err := sender.SendMessage(ctx, message, nil); err != nil {
		log.Fatalf("failed to send: %v", err)
	}
	fmt.Printf("sent %q to %s\n", body, queueName)
}
