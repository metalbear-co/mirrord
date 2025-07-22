# mirrord Style Guide

## Rust

- When naming Senders and Receivers, use `a_tx` and `b_rx` respectively - where `a` is the thing the Sender is sending, and `b` is the thing Receiver is receiving. E.g. `agent_message_tx`, `http_response_rx`.
    - Document above the declaration of the variable/struct member what’s being sent from where to where. Suggested format:
      `<src> --<what's being sent>—> <dst>`.

      Example:
      `/// HTTP client task —-IDs of closed connections—-> TcpStealHandler`
      If known, the meaning of the objects being sent is documented, not just their type.
- Don’t use “local” in naming in agent code, since it could be unclear if it’s local to the agent, or to the user’s system. Instead, use “agent”, “cluster”, “pod”, “container”, “layer”, “user_application” to specify where this item is local to.
- When using `unwrap` or `expect`, always explain why it’s ok to do so in a comment (except for in tests obviously).
- Only use `Option`s where it is valid to have a `None`. Avoid passing around
  `Option`s that are expected to always be `Some`, and hold the inner value
  instead as early as possible.
  - That way, developers don't have to make sure that the `Option` that is
    expected to be `Some` is not `None` at later points along the run.
  - Also, the code is easier to read this way: you don't have to somehow know
    that an `Option` is expected to never be `None` in order to understand the
    code.
