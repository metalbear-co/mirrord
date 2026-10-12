# mirrord-db-branching

Configure mirrord for isolated database branches during development. (Team / Enterprise feature.)

## What it does

This skill helps AI agents:
- **Generate** valid `feature.db_branches` configs for mirrord.json
- **Configure** MySQL, MariaDB, PostgreSQL, MSSQL, MongoDB, Redis, DynamoDB, ClickHouse, Google Spanner, Amazon S3, and Generic branches
- **Set up** copy modes (empty, schema, all, filtered) and connection sources (env, Kubernetes Secret, ConfigMap, Google Secret Manager, AWS Secrets Manager, literal, composite, multiple)
- **Run** schema migrations against a branch (Flyway or Liquibase)
- **Configure** IAM authentication for AWS RDS, GCP Cloud SQL, and DynamoDB
- **Manage** database branches via CLI commands

## Example prompts

```
"Set up a MySQL database branch for testing migrations"

"Configure mirrord to use a PostgreSQL branch with schema copy"

"Run my Flyway migrations against a Postgres branch"

"Set up a DynamoDB branch with a full copy of my tables"

"Help me set up a local Redis instance with mirrord"

"Branch a service that has no built-in engine using my own image"

"Configure DB branching with AWS RDS IAM authentication"

"How do I filter which rows get copied to my database branch?"

"Branch an S3 bucket so my app writes to a throwaway clone"

"Branch a turbopuffer namespace for testing"
```

## Supported databases

| Database | `type` | Branch location | Notes |
|----------|--------|-----------------|-------|
| MySQL | `"mysql"` | Remote | IAM auth, migrations, `dump_args` |
| MariaDB | `"mariadb"` | Remote | IAM auth, migrations |
| PostgreSQL | `"pg"` | Remote | IAM auth, migrations, `dump_args`, `connection_settings`, `query_params` |
| MSSQL | `"mssql"` | Remote | migrations |
| MongoDB | `"mongodb"` | Remote | collections (no `schema` mode) |
| Redis | `"redis"` | Remote or local | `name` = DB index |
| DynamoDB | `"dynamodb"` | Remote (local emulator) | IAM required for full copy |
| ClickHouse | `"clickhouse"` | Remote | |
| Google Spanner | `"spanner"` | Remote (emulator) | uses `SPANNER_EMULATOR_HOST` |
| Amazon S3 | `"s3"` | Remote (your cloud account) | not a pod; `source.params.bucket`, `provider` |
| turbopuffer | `"turbopuffer"` | Remote (your turbopuffer account) | not a pod; `source.params.namespace`/`api_key`/`region` |
| Generic | `"generic"` | Remote | any service, your own image |

## Quick example

```json mirrord
{
  "feature": {
    "db_branches": [
      {
        "type": "pg",
        "version": "16",
        "connection": { "url": "DATABASE_URL" },
        "copy": { "mode": "schema" }
      }
    ]
  }
}
```

> `db_branches` is nested under the top-level `feature` object.

## Branch management

```bash
# View branch status
mirrord db-branches status

# Destroy a branch / all branches
mirrord db-branches destroy <branch-name>
mirrord db-branches destroy --all

# List active branch portforwards (while a session is running)
mirrord db-branches connections
```

## References

This skill uses local reference files:
- [`../mirrord-config/references/schema.json`](../mirrord-config/references/schema.json): mirrord JSON Schema, synced from upstream; `db_branches` lives under `feature`. The fence tag check validates config examples against it; read it only as a last resort, when neither `validate_config` (mirrord MCP server) nor `mirrord verify-config` is available
- `references/troubleshooting.md` — common issues and solutions

## Learn more

- [DB Branching Overview](https://metalbear.com/mirrord/docs/sharing-the-cluster/db-branching/)
- [Connection Modes](https://metalbear.com/mirrord/docs/sharing-the-cluster/db-branching/connection/)
- [IAM Authentication](https://metalbear.com/mirrord/docs/sharing-the-cluster/db-branching/iam-authentication/)
- [Schema Migrations](https://metalbear.com/mirrord/docs/sharing-the-cluster/db-branching/migrations/)
- [Branch Management](https://metalbear.com/mirrord/docs/sharing-the-cluster/db-branching/management/)
