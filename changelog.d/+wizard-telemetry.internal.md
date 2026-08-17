The config wizard now reports its failures to telemetry: a `wizard_user_blocked` event fires
when a wizard API query settles into an error state (once per failure episode) or when the
wizard UI crashes, with exception tracking attached, and `wizard_opened` marks wizard
activations. Previously the wizard emitted no telemetry at all, so wizard-blocking failures
were invisible.
