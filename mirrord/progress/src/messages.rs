//! Progress messages texts in the form of `(id, text)`, where we use the `id` to allow
//! the user to disable this type of notification.

/// Warning when user selects a multi-pod deployment without MfT.
pub const MULTIPOD_WARNING: (&str, &str) = (
    "multipod_warning",
    "When targeting multi-pod deployments, mirrord impersonates the \
        first pod in the deployment. \
        Support for multi-pod impersonation requires the mirrord operator, \
        which is part of mirrord for Teams.",
);

/// Warning when user is using an HTTP filter without MfT.
pub const HTTP_FILTER_WARNING: (&str, &str) = (
    "http_filter_warning",
    "You're using an HTTP filter, which generally indicates \
        the use of a shared environment. If so, we recommend \
        considering mirrord for Teams, which is better suited \
        to shared environments.",
);

/// Shown wherever a run is told it needs the operator. An AI coding agent cannot use the sign-up
/// web app those messages link to, but it can fetch a plain-text page and follow it.
pub const AGENT_OPERATOR_HINT: &str = "AI coding agents can install the mirrord operator \
    autonomously, no account needed: https://metalbear.com/agents.md";

/// Warning when user tries to run `mirrord exec docker` (for example), instead of the correct
/// `mirrord container ...`.
pub const EXEC_CONTAINER_BINARY: &str = "`mirrord exec <docker|podman|nerdctl> ...` detected! \
    If you try to run a container with mirrord, please use \
    `mirrord container [options] -- <docker|podman|nerdctl> ...` instead.";

/// Final success message printed once a `mirrord exec` / `mirrord port-forward`
/// session is fully initialized (for `exec`, once the layer has started in the
/// user's program; on Linux and macOS the layer prints it itself in `simple` and
/// `json` modes, and `exec` prints it before `execve` in a terminal).
///
/// `mirrord up` parses child stdout for this exact line (in `simple` progress
/// mode) to detect when a session is ready. Keep the emitters and that parser
/// in sync via this constant.
pub const SESSION_READY_MESSAGE: &str = "Ready!";

/// The task `mirrord exec` runs the user's program under, which finishes with
/// [`SESSION_READY_MESSAGE`].
///
/// On Linux and macOS in `json` mode the layer finishes it from inside the
/// program, and the IDE matches the finish to the task by this name.
pub const EXEC_RUNNING_TASK: &str = "running process";

/// The note printed under a share link: what people who open it reach, and who can open it.
///
/// share-ingress forwards every request for the link to the target's Service, so whenever the
/// session doesn't steal a request (the application isn't listening, the CLI has exited but the
/// session still lives, or the request lands on a pod without the session's agent) the cluster's
/// own service answers it through the public share domain. The note says so, because the share
/// domain's access rules then guard that service too.
pub const LOCAL_SHARE_NOTE: &str = "Note: people who open this link reach the service running on your machine, and the one running in the cluster whenever a request doesn't reach your machine. Who can open it is set by your cluster's access rules for share links.";
