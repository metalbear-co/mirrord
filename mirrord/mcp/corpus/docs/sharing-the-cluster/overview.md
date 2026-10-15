---
title: Sharing the Cluster
draft: false
images: []
toc: true
description: >-
  How mirrord makes it possible for developers to use the same cluster
  concurrently.
tags:
  - team
  - enterprise
---

# Overview

The core value of mirrord is that it cuts iteration time by letting developers run their code against the cluster directly, instead of having to build, push and deploy images. This significantly cuts down iteration time by letting developers test their code in the cloud from the very first step of the development cycle. However, in order to properly test new code in the cloud, it needs to be able to not only read or receive traffic from the environment, but also to write or send traffic to it, potentially mutating it. This discussion is only relevant for users on the Team and Enterprise pricing plans.

This raises the question, what if I want multiple users in my organization to use the same cluster (e.g. the organization's staging cluster) concurrently? Wouldn't they step on each other's toes and affect each other's work?

1. If one developer steals traffic from a remote service, wouldn't that prevent other users from stealing or mirroring traffic from that same service?
2. If a service reads from a queue, wouldn't a developer targeting it with mirrord steal all the messages from the queue, preventing other developers from reading them?
3. If a developer writes to a database, wouldn't that affect the data that other developers see when they read from the same database?

These conflicts and more are resolved by the mirrord Operator, available in the mirrord Team and Enterprise pricing plans. By having a persistent, centralized component in the cluster that can synchronize and orchestrate different instances of mirrord running in the cluster, we can allow developers to use mirrord against the same cluster without affecting each other.

### What capabilities does mirrord have to allow concurrent usage of the same cluster?

![Using clusters concurrently with mirrord](../.gitbook/assets/shared-cluster.png)

#### 1. Concurrently debug the same HTTP server with HTTP filters

mirrord's HTTP filters let users only steal a subset of the incoming traffic to the remote service. By adding personalized headers to incoming traffic and then configuring mirrord to only steal traffic with those headers, users can debug the same service concurrently without affecting each other. [Learn more about HTTP filters](../using-mirrord/incoming-traffic/filter-incoming-traffic.md).

> _**NOTE:**_ While HTTP filters are supported in the OSS version of mirrord, concurrently debugging the same service using HTTP filters is only supported in the Team and Enterprise pricing plans.

#### 2. Concurrently debug the same queue-based service with queue splitting

mirrord's queue splitting feature lets users only steal a subset of the messages from a queue. By configuring mirrord to only steal messages with specific properties, users can debug the same queue-based service concurrently without affecting each other. [Learn more about queue splitting](queue-splitting.md).

#### 3. Prevent unwanted behavior with mirrord Policies

mirrord Policies let you define rules that prevent users from doing certain actions. For example, you can prevent users from writing to a database, or from stealing traffic without using an HTTP filter. [Learn more about mirrord Policies](policies.md).

#### 4. Communicate with a personal isolated database

Sometimes a database is just too sensitive to write to remotely. Or maybe you want to test a migration, and don't want it to affect your coworkers who are using the same cluster. In these cases, you can use the DB Branching feature to work with an ephemeral database branch that is isolated from the main database. [Learn more about DB Branching](db-branching.md).

#### 5. View other sessions running in the cluster (and kill them if necessary)

Sometimes, all you need to avoid clashes is just to see what other users are doing in the cluster. The `mirrord operator status` command displays a list of all the currently running sessions in the cluster, along with the user who started them. If you see a session that's causing problems, you can kill it using the `mirrord operator session kill --id {id}` command (given you have the necessary permissions). [Learn more about managing mirrord sessions](sessions.md).

#### 6. Guardrails for Destructive Actions in mirrord

To prevent destructive operations (like DB resets during integration tests) from running against remote environments, mirrord supports `MIRRORD_DONT_LOAD` environment variable. When set to `true`, mirrord will immediately abort and refuse to start, regardless of IDE settings. This makes it easy to mark specific scripts or run configurations (e.g. integration tests) as "safe" from mirrord.
