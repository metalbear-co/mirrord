Added the `mirrord chaos` command for managing chaos rules. The available subcommands are `list`,
`add`, `edit` and `delete`. Silently starts the local UI if needed. New rules can be provided as a
file or to `stdin`, and output can be JSON format or pretty printed.
