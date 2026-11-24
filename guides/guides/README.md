---
description: Step-by-step guides for debugging microservices, testing AI-generated code, and running AI agents in Kubernetes with mirrord
layout:
  pagination:
    visible: false
---

# Guides

These guides walk you through debugging real applications running in Kubernetes using [mirrord](https://metalbear.com/mirrord/). Each guide includes a sample application, step-by-step IDE and CLI setup, and a comparison with traditional debugging techniques.

## Language Guides

Debug microservices written in your language of choice, with IDE integration and CLI workflows.

| Language | IDE | Guide |
|----------|-----|-------|
| **Java** | IntelliJ IDEA | [How to Debug Java Microservices](how-to-debug-a-java-microservice.md) |
| **Go** | GoLand | [How to Debug Go Microservices](how-to-debug-a-go-microservice.md) |
| **Node.js** | VS Code | [How to Debug Node.js Microservices](how-to-debug-a-nodejs-microservice.md) |
| **Kotlin** | IntelliJ IDEA | [How to Debug Kotlin Microservices](how-to-debug-a-kotlin-microservice.md) |
| **Ruby** | RubyMine | [How to Debug Ruby Microservices](how-to-debug-a-ruby-microservice.md) |
| **PHP** | CLI | [How to Debug PHP Microservices](how-to-debug-a-php-microservice.md) |
| **Python** | VS Code | [How to Debug Python Microservices](how-to-debug-a-python-microservice.md) |
| **.NET** | VS Code | [How to Debug .NET Microservices](how-to-debug-a-dotnet-microservice.md) |

## Queue Guides

Debug queue consumers with mirrord's copy target and queue splitting features.

| Queue | Guide |
|-------|-------|
| **Kafka** | [How to Debug Kafka Consumers](how-to-debug-kafka-consumers.md) |
| **SQS** | [How to Debug SQS Consumers](how-to-debug-sqs-consumers.md) |

## AI Guides

Use mirrord to test AI-generated code and run AI coding agents against your Kubernetes cluster.

| Guide | Description |
|-------|-------------|
| **[How to Set Up AI Tools with mirrord](setting-up-mirrord-for-ai-tools.md)** | Configure Cursor, Claude Code, Copilot, Codex, and Windsurf |
| **[How to Test AI-Generated Code with mirrord](testing-ai-generated-code.md)** | Validate AI-generated code against your Kubernetes cluster |
| **[Autonomous AI Workflows with mirrord](running-ai-agents-with-mirrord.md)** | Set up E2E guardrails so agents validate their own changes |

***

{% hint style="info" %}
All guides use a sample Guestbook or message-processing application. You can find the source code linked in each guide.
{% endhint %}
