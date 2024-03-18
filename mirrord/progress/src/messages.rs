//! Progress messages texts in the form of `(id, text)`, where we use the `id` to allow
//! the user to disable this type of notification.

/// Warning when user selects a multi-pod deployment without MfT.
pub const MULTIPOD_WARNING: (&str, &str) = (
    "multipod-warning",
    "When targeting multi-pod deployments, mirrord impersonates the \
        first pod in the deployment.\n\n\
        Support for multi-pod impersonation requires the mirrord operator, \
        which is part of mirrord for Teams.\n\n\
        You can get started with mirrord for Teams at this link: \
        https://mirrord.dev/docs/overview/teams/",
);
