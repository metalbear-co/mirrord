Report unhandled errors in the local UI as user-facing failures rather than background
health signals, so they sit alongside the error-boundary crash they are a variant of, and
drop the event-kind property that no longer distinguishes anything.
