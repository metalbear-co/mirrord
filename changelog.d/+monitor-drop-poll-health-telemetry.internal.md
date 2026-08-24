Stop emitting session-poll health telemetry from the local UI. The events fired on
every transition of a poll that recovers on its own, and reported no duration, so they
could not distinguish a momentary blip from a sustained outage.
