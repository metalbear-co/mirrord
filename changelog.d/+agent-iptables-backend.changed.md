Changed semantics of the `agent.nftables` configuration field.

When the field is not set, the agent will pick between `iptables-legacy` and `iptables-nft` at runtime,
based on kernel support and existing mesh rules.

When the field is set, the agent will always use either `iptables-legacy` or `iptables-nft`.
