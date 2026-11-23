# How to Debug Kafka Consumers

In this guide, we’ll cover how to debug Kafka consumer applications running in a Kubernetes environment using mirrord. You’ll learn how to set up mirrord and use it to effectively debug Kafka consumers without the traditional overhead of rebuilding and redeploying your application.

**Tip**: mirrord is a development tool which makes developing cloud applications significantly easier. If you're not familiar, we recommend checking out the [getting started guide](https://metalbear.com/mirrord/docs/overview/quick-start/) first!

Debugging distributed applications, especially those that use messaging systems like Apache Kafka, can be challenging. These systems often span multiple services and depend on asynchronous communication patterns. Traditional debugging approaches can fall short when trying to trace issues across these complex systems.

Developers can debug cloud-native applications using mirrord by allowing local Kafka consumer applications to access remote Kubernetes resources using context mirroring. Let’s go through some common techniques to debug such applications to understand why using mirrord would help you save time and shorten the development feedback loop when working with Kafka consumers.

## Common debugging techniques for Kafka consumers

It can be cumbersome to debug Kafka consumers on Kubernetes. The lack of a debugging workflow for applications with multiple runtime dependencies in the context of Kubernetes makes it even harder. Why is debugging Kafka consumers in Kubernetes so challenging? Let’s look at the common approaches and their limitations:

### Continuous Deployment

Build a container image and deploy it to a Kubernetes cluster dedicated to testing or staging. The iterative process of building, deploying, and testing is resource-intensive and time-consuming, especially when you’re making frequent code changes to your Kafka consumers.

### Log Analysis

One of the most common ways to understand Kafka consumer behavior in a cluster is by analyzing logs. Adding extra logs to extract runtime information about message consumption, offsets, and processing is very common. While collecting and analyzing logs from different consumers can be effective, it isn’t the best real-time debugging solution, especially when trying to trace message flow through the system.

### Remote Debugging

Developers can use remote debugging tools built into IDEs to attach to Kafka consumer processes already running in a Kubernetes cluster. While this allows real-time code inspection and interaction, it still requires heavy overhead from the IDE and a separate debug configuration for the deployment which can potentially affect the consumer’s performance while debugging.

The above methods can be used by themselves or they can be used together, but they all have significant drawbacks in terms of development speed and efficiency.

## Challenges of debugging Kafka consumers in Kubernetes

Debugging Kafka consumers effectively within a Kubernetes context is perhaps the biggest challenge of working with Kafka in cloud environments. The build and release loop of the application can be short, but the process slows down development significantly. Nothing beats the ease and speed of debugging applications locally.

Kafka consumers present unique challenges:

1. Message Distribution: Kafka distributes messages among all consumers in a consumer group, making it difficult to ensure your debug consumer sees all relevant messages.

1. State Management: Consumers maintain offset state, and debugging can disrupt this state management, leading to missed or duplicate message processing.

1. Environment Dependencies: Kafka consumers typically depend on specific configurations for brokers, topics, and security settings that must be replicated in the debugging environment.

1. Performance Implications: Debugging tools can introduce latency that may trigger rebalancing or timeout issues that don’t exist in production.

1. Distributed Tracing: Following a message through the entire system from producer to consumer can be challenging without proper tooling.

These challenges make it essential to have a debugging approach that can seamlessly integrate with both local and remote Kafka environments.

## Prerequisites

Before we dive into debugging Kafka consumers with mirrord, ensure you have the following set up:

### Developer Environment

A Kubernetes cluster (you can use one of the following):

- kind (Kubernetes in Docker)
- minikube
- Remote cluster with proper access

Tools:

- kubectl CLI
- mirrord CLI
- helm (if using helm for mirrord operator installation)

## Installing mirrord

Install the mirrord CLI:

```bash
brew install metalbear-co/mirrord/mirrord
```

For alternative installation methods, please follow the [quick start guide](https://metalbear.com/mirrord/docs/overview/quick-start/).

## Installing mirrord operator

You can install the mirrord operator using either Helm or the mirrord CLI:

**Tip**: You can obtain a LICENSE key by visiting https://app.metalbear.co/. This key is required for installing the mirrord operator with queue splitting functionality.

### Using Helm:

```bash
# Add the MetalBear Helm repository
helm repo add metalbear https://metalbear-co.github.io/charts

# Install the mirrord operator with SQS splitting enabled
helm install --set license.key=your-license-key --set operator.kafkaSplitting=true mirrord-operator metalbear/mirrord-operator
```

**Note**: When installing with the mirrord-operator Helm chart, queue splitting is enabled by setting the `operator.kafkaSplitting` value to true.

### Or using the CLI:

```bash
mirrord operator setup --accept-tos --license-key your-license-key --kafka-splitting --aws-role-arn 12345 | kubectl apply -f -
```

## Sample application setup

### Overview of the sample application

Let’s explore how to debug Kafka consumers using a simple example application. Our example consists of:

- A Kafka broker

- A producer service that publishes messages to a Kafka topic

- A consumer service that reads messages from the same topic

**Note:** The sample application used in this guide is available [here](https://github.com/waveywaves/mirrord-kafka-debug-example). This guide is intended to be used alongside the repository, which contains all the code and configuration files needed to follow along.

The following architecture diagram shows the basic setup of our Kafka application in Kubernetes without mirrord. It illustrates how the producer sends messages to the Kafka broker, and how the consumer reads these messages in a standard deployment:

![Architecture Diagram - Setup without mirrord](how-to-debug-kafka-consumers/setup-without-mirrord.png)

### Understanding the application components

Our example application has these components:

**Producer** (`kafka-producer` deployment):

- Flask web application that publishes messages to a Kafka topic
- Exposes an API endpoint for receiving message data
- Uses the confluent-kafka Python library

**Consumer** (`kafka-consumer` deployment):

- Flask web application that reads messages from the Kafka topic
- Processes messages and exposes them in the logs
- Uses the confluent-kafka Python library

**Kafka Configuration**:

- Single broker Kafka cluster
- Topic: test-topic
- Consumer group: test-consumer-group

The application components are containerized and deployed to Kubernetes.

## Deploying the sample application

### Starting your Kubernetes cluster

We are going to use minikube as the Kubernetes environment in which we are going to deploy our sample application. Run the following command to start minikube locally.

```bash
minikube start
```

**Note**: We're only using a local cluster for this sample app and actually the idea is for developers to use mirrord with a staging/testing cluster where the app is already deployed.

### Applying Kubernetes manifests

Let’s start by deploying our example Kafka application to Kubernetes:

```bash
kubectl apply -f kube/
```

After running this command, you’ll see output similar to this:

![Kubernetes Apply Command](how-to-debug-kafka-consumers/k8s-apply-output.png)

This deploys the Kafka broker, producer, and consumer to your Kubernetes cluster. Once everything is up and running, we can proceed with debugging.

### Important Kafka configuration values

#### Kafka endpoint configuration

| Parameter               | Value                                        | Description                                           |
| :---------------------- | :------------------------------------------- | :---------------------------------------------------- |
| bootstrap.servers       | kafka-0.kafka.default.svc.cluster.local:9092 | Kafka broker address                                  |
| security.protocol       | PLAINTEXT                                    | Authentication protocol (no security in this example) |
| client.id               | mirrord-operator                             | Client identifier for the Kafka connection            |
| request.timeout.ms      | 20000                                        | Request timeout in milliseconds                       |
| connections.max.idle.ms | 300000                                       | Maximum time connections can remain idle              |

#### Consumer configuration

| Parameter          | Value               | Description                                  |
| :----------------- | :------------------ | :------------------------------------------- |
| Topic Name         | test-topic          | The Kafka topic the consumer subscribes to   |
| Group ID           | test-consumer-group | Consumer group identifier for the consumer   |
| Partition Strategy | Auto (default)      | How messages are distributed among consumers |
| Auto Commit        | Enabled             | Whether offsets are committed automatically  |

## Debugging a Kafka consumer with mirrord

### Using the Kafka Producer UI to send messages to the consumer

Before we start debugging the consumer itself we need to know how to send messages to the Kafka topic in question. This can be done by accessing the UI for the consumer using port forwarding. Running the following command will allow you to access your Kafka Producer UI in the browser at http://localhost:8080

kubectl port-forward deployment/kafka-producer 8080:

Once you navigate here using your browser you should be able to access the producer UI.

![Kafka Producer UI](how-to-debug-kafka-consumers/producer-ui.png)

Now that we know how to access the producer, let’s dive into two effective approaches for debugging Kafka consumers with mirrord:

### Approach 1: Simple Debugging with copy_target + scaledown (easier to execute)

The simplest way to debug Kafka consumers is to ensure your local consumer is the only one receiving messages from the topic. mirrord’s copy_target feature with scale_down enabled accomplishes this. For detailed documentation, see [mirrord’s copy target documentation](https://metalbear.com/mirrord/docs/using-mirrord/copy-target/#replacing-a-whole-deployment-using-scale_down).

```json
{
  "operator": true,
  "target": {
    "deployment": "kafka-consumer",
    "container": "consumer"
  },
  "feature": {
    "copy_target": {
      "scale_down": true
    }
  }
}
```

This configuration:

1. Targets the `kafka-consumer` deployment

2. Creates a copy of the deployment’s pod

3. Scales down the original deployment to zero replicas

4. Ensures your local application receives all the messages

This approach is illustrated in the following diagram:

![Architecture Diagram - Copy Target with Scale Down](how-to-debug-kafka-consumers/arch-copy-target-with-scale-down.png)

1. Here the kafka-producer sends messages to the test-topic topic

2. Which is then picked up by the mirrord-copy Pod which has scaled the consumer Pod down and is going to relay the message to our local consumer.

3. The local mirrord based execution of the consumer application receives this. Messages can be created and sent from the UI

Now that we understand how this works, let’s use this configuration locally.

#### How to use this configuration

The configuration is located in a file named `.mirrord/copytarget_plus_scaledown.json`. Use this configuration to run the consumer application locally with the following command:

```bash
APP_MODE=consumer PYTHONUNBUFFERED=1 mirrord exec --config .mirrord/copytarget_plus_scaledown.json -- python app.py
```

When you run this command, you can use the producer to send messages which will then be picked up by your local consumer after which, you’ll see output similar to:

![Terminal Output for Copy Target](how-to-debug-kafka-consumers/terminal-copy-target.png)

**Tip:** This approach is perfect for isolated debugging, but be aware that it temporarily stops the original consumer from processing messages. Use it in development or testing environments rather than production.

### Approach 2: Queue Splitting for non-disruptive debugging

Queue splitting is a powerful feature in mirrord that allows both your local application and the remote application to receive the same messages. This is particularly useful when you want to debug without disrupting the existing remote consumers. For detailed documentation on queue splitting, visit [https://metalbear.com/mirrord/docs/sharing-the-cluster/queue-splitting](https://metalbear.com/mirrord/docs/sharing-the-cluster/queue-splitting).

#### How queue splitting works

Before we get into how to use queue splitting, let’s go through the following diagrams to illustrate how queue splitting works in mirrord:

Initial setup with the mirrord operator intercepting messages:

![Architecture Diagram - Queue Splitting with single debug consumer](how-to-debug-kafka-consumers/queue-splitting-single.png)

The mirrord operator intercepts messages at the Kafka broker level before they are delivered to consumers. It makes copies of these messages and delivers them to both the remote consumers and your local application.

This works by:

1. The mirrord operator understands the Kafka protocol
2. It identifies messages being sent to specific topics
3. It duplicates these messages, sending the original to the real consumers and copies to your local application

When multiple debug consumers are active, the mirrord operator creates temporary queues for each one. Each developer gets their own independent debug session with access to the message stream, allowing multiple developers to use the same shared cluster for development and testing without worrying about using other tools for multi-tenancy.

In case of multiple debug consumers, a new temporary mirrord topic and mirrord-copy Pod is created for every new debug consumer. Setup with two distinct debug consumers:

![Architecture Diagram - Queue Splitting with two distinct consumers](how-to-debug-kafka-consumers/queue-splitting-multiple.png)

mirrord allows multiple debug consumers to run simultaneously. Each developer can run their own local consumer, and all will receive copies of the same messages. The mirrord operator also ensures that each local debug consumer gets a complete copy of the message stream, without any competition between them or with the production consumers.

**Tip:** This means your entire team can debug the same Kafka consumer application simultaneously without interfering with each other or with other traffic to the staging cluster! This team collaboration capability is a feature of [mirrord for Teams](https://metalbear.com/mirrord/docs/overview/teams/).

#### Configuration for queue splitting

For queue splitting to work, you need to have a local and a remote configuration for debugging your Kafka based application. The local configuration is used by the CLI and the remote configuration contains CRDs which are used by the operator to get more information on the Kafka instance and the topics to be intercepted.

##### Local mirrord configuration

The queue splitting configuration can be found in the .mirrord/mirrord.json file and has the following content:

```json
{
  "operator": true,
  "target": {
    "deployment": "kafka-consumer",
    "container": "consumer"
  },
  "feature": {
    "split_queues": {
      "test_topic": {
        "queue_type": "Kafka",
        "message_filter": {
          "source": "^test-.*"
        }
      }
    }
  }
}
```

This configuration tells the local mirrord client:

- The Kafka topic to listen to (`test_topic`)
- To filter messages based on a pattern (`^test-`)
- Which deployment to target (`kafka-consumer`)

##### Operator configuration

The mirrord operator needs information about the Kafka setup. This is configured using Kubernetes custom resources.

First, create a `MirrordPropertyList` resource holding the connection settings for the operator's own Kafka client. Each property is passed straight to the underlying Kafka client:

```yaml
apiVersion: mirrord.metalbear.co/v1
kind: MirrordPropertyList
metadata:
  name: base-config
  namespace: default
spec:
  properties:
    - name: bootstrap.servers
      value: kafka-0.kafka.default.svc.cluster.local:9092
    - name: security.protocol
      value: PLAINTEXT
```

Next, create a `MirrordSplitConfig` resource:

```yaml
apiVersion: queues.mirrord.metalbear.co/v1
kind: MirrordSplitConfig
metadata:
  name: kafka-consumer-split
  namespace: default
spec:
  targetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: kafka-consumer
  queues:
    - id: test_topic
      kind: kafka
      clientConfig: base-config
      appConfig:
        topic:
          - env: KAFKA_TOPIC
            containers:
              - consumer
        groupId:
          - env: KAFKA_GROUP_ID
            containers:
              - consumer
```

This `MirrordSplitConfig` says that:

- It targets the `kafka-consumer` deployment (`spec.targetRef`).
- The `consumer` container reads the topic name from the `KAFKA_TOPIC` environment variable and the consumer group ID from `KAFKA_GROUP_ID`.
- The topic is referenced from the local mirrord configuration under the ID `test_topic` (the queue IDs in `split_queues` have to match the queue IDs in the `MirrordSplitConfig`).
- The operator connects to Kafka using the settings from the `base-config` `MirrordPropertyList`.

For the full resource reference, see the [Kafka queue splitting documentation](https://metalbear.com/mirrord/docs/sharing-the-cluster/queue-splitting/kafka).

**Note**: `MirrordSplitConfig` requires mirrord operator `3.170.0` or later and mirrord CLI `3.221.0` or later. It replaces the deprecated `MirrordKafkaTopicsConsumer` and `MirrordKafkaClientConfig` resources; if you have existing resources of the old kinds, see [Migrating to MirrordSplitConfig](https://metalbear.com/mirrord/docs/sharing-the-cluster/queue-splitting/migrating-to-mirrordsplitconfig).

Save both resources above to a file and apply them to your cluster:

```bash
kubectl apply -f kafka-split-config.yaml
```

**Note**: The sample repository's `kube/` manifests still contain the legacy `MirrordKafkaTopicsConsumer` and `MirrordKafkaClientConfig` resources. The resources above replace them, so apply them yourself as shown here.

#### Running your local consumer with queue splitting

Now you can run your local application with mirrord using queue splitting:

```bash
APP_MODE=consumer PYTHONUNBUFFERED=1 mirrord exec -f .mirrord/mirrord.json -- python app.py
```

**Tip:** You can use message filters to focus on specific patterns of messages, making debugging more targeted.

After running with queue splitting and specific message filtering, you’ll see output like this:

![Terminal Screenshot for Filter Queue Splitting](how-to-debug-kafka-consumers/terminal-queue-splitting.png)

You’ll see that the consumer and the copy Pods are both available ensuring that the original consumer doesn’t stop consuming the messages whereas the copy Pods will receive the filtered messages:

![mirrord Exec Queue Splitting 1](how-to-debug-kafka-consumers/terminal-single.png)

Similarly multiple debug consumers can consume these messages without disrupting the original consumer by creating more copy Pods and temporary mirrord topics as required:

![mirrord Exec Queue Splitting 2](how-to-debug-kafka-consumers/terminal-multiple.png)

Here, each instance creates its own mirrord-copy pod and receives the same messages, demonstrating how multiple developers can debug simultaneously. Notice in the second screenshot how multiple debug consumers are actively receiving messages in parallel. This collaborative debugging is made possible by [mirrord for Teams](https://metalbear.com/mirrord/docs/overview/teams/), which enables concurrent use of mirrord on the same environment.

## Debugging with mirrord vs. other debugging techniques

mirrord distinguishes itself by eliminating the need for repeated building and deployment cycles. It allows you to run your Kafka consumer locally while providing it with the necessary network and execution context of the target Kubernetes environment. Your local consumer behaves as if it were running within the cluster, enabling you to debug using familiar tools without the overhead of build and deploy cycles.

## Conclusion {#conclusion}

In this guide, we’ve explored how to use mirrord to debug Kafka consumer applications in Kubernetes. We’ve seen two powerful approaches:

1. **Queue splitting** allows you to debug without disrupting existing consumers by duplicating messages. Learn more about this feature in the [queue splitting documentation](https://metalbear.com/mirrord/docs/sharing-the-cluster/queue-splitting).

2. **Copy target with scale down** gives your local application exclusive access to Kafka messages. Learn more in the [copy target documentation](https://metalbear.com/mirrord/docs/using-mirrord/copy-target/#replacing-a-whole-deployment-using-scale_down).

By leveraging mirrord, you can significantly improve your productivity when working with Kafka consumer applications and streamline your debugging workflow. For teams working together on Kafka applications, [mirrord for Teams](https://metalbear.com/mirrord/docs/overview/teams/) provides additional collaborative features that enable multiple developers to debug simultaneously.

Curious to try it out? Give mirrord a go and see how it works for you. Got questions? Visit the [mirrord documentation](https://metalbear.com/mirrord/docs/) or join the community [Slack](https://metalbear.com/slack) channel!
