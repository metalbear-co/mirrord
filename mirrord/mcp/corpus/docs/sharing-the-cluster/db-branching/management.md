---
title: Branch Management
description: How to manage isolated mirrord DB branches for visibility and control
tags:
- beta
- team
- enterprise
---

mirrord provides CLI commands to inspect and manage database branches:

| Command | Purpose |
| --- | --- |
| `mirrord db-branches status [name...]` | Show the status of running database branches |
| `mirrord db-branches destroy (--all \| name...)` | Destroy one or more running database branches |
| `mirrord db-branches connections` | List the currently active DB branch portforwards |

All commands accept `(-n\|--namespace) namespace` to select a namespace and `-A\|--all-namespaces` to operate across all namespaces.

## View Branch Status

```bash
mirrord db-branches [(-n|--namespace) namespace] [-A | --all-namespaces] status [name...]
```

* Shows the status of running database branches.
* If specific branch names are provided, mirrord shows their statuses.
* If no names are given, all active branches in the selected namespace (or all namespaces, if --all-namespaces is used) are listed.
* If no branches are active, mirrord returns:`No active DB branch found`

Example output:

| Name | Pod Name | DB Type | Phase | TTL (sec) | Database | Users | Expires At |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `mirrord-pg-branch-070081bc40967ac9` | `mirrord-postgresql-branch-db-pod-686qf` | PostgreSQL | Ready | 600 | `my_database_name_etc` | `minikube-user` | `2026-07-06T04:36:59.683405Z` |

## Destroy Branches

```bash
mirrord db-branches [(-n|--namespace) namespace] [-A | --all-namespaces] destroy (--all | name...)
```

* Destroys one or more running database branches.
* Use `--all` to destroy every active branch.
* Use one or more branch names to target specific branches.
* mirrord uses the default namespace, or the namespace specified with `--namespace`.
* To destroy branches across all namespaces, use `--all-namespaces`.
* If no active branches are found, mirrord returns: `Error: No active DB branch found`

## List Branch Portforwards

```bash
mirrord db-branches connections
```

* Lists the currently active DB branch portforwards, which are set up automatically while a session is active. See [Portforwards](../db-branching.md#portforwards).

Example output:

| DB ID | Address | Key | Session ID |
| --- | --- | --- | --- |
| `pg-dev-random-id10` | `postgresql://postgres:postgres-branch-pod-root-password@[::]:53778/my_database_name_etc` | `40c85` | `5E68BB2A295FDC5E` |

When no session is active:

```text
No active portforward sessions.
```
