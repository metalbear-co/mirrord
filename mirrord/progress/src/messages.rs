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

/// Warning when user tries to run `mirrord exec docker` (for example), instead of the correct
/// `mirrord container ...`.
pub const EXEC_CONTAINER_BINARY: &str = "`mirrord exec <docker|podman|nerdctl> ...` detected! \
    If you try to run a container with mirrord, please use \
    `mirrord container [options] -- <docker|podman|nerdctl> ...` instead.";

/// Final success message printed once a `mirrord exec` / `mirrord port-forward`
/// session is fully initialized (for `exec`, immediately before the user's
/// binary is `execve`d).
///
/// `mirrord up` parses child stdout for this exact line (in `simple` progress
/// mode) to detect when a session is ready. Keep the emitters and that parser
/// in sync via this constant.
pub const SESSION_READY_MESSAGE: &str = "Ready!";
