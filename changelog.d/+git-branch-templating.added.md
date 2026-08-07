Configuration templating now exposes a `git_branch` variable holding the current git branch, so a
config can derive values from it, for example giving each branch its own session key. Outside a git
checkout the variable stays undefined, so pair it with the `default` filter when the same config
also has to work there.
