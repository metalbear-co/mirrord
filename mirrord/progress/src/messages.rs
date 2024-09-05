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

/// Warning when user tries to run `mirrord exec docker` (for example), instead of the correct
/// `mirrord container ...`.
pub const EXEC_CONTAINER_BINARY: &str = "`mirrord exec <docker|podman|nerdctl> ...` detected! \
    If you try to run a container with mirrord, please use \
    `mirrord container [options] -- <docker|podman|nerdctl> ...` instead.";
