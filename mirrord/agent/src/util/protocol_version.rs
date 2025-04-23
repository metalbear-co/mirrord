use std::sync::{Arc, Mutex};

/// Shared and cloneable [`mirrord_protocol`] version of an agent client.
///
/// Client's [`mirrord_protocol`] is used in multiple places.
/// Storing it in behind a shared wrapper allows us to simplify the code
/// and avoid passing it around in messages.
///
/// Its [`Default`] implementation sets the version to `0.1.0`.
/// This is a dummy value that should not match any [`semver::VersionReq`] used in
/// [`mirrord_protocol`], but still matches [`semver::VersionReq::STAR`].
#[derive(Clone, Debug)]
pub struct SharedProtocolVersion(Arc<Mutex<semver::Version>>);

impl Default for SharedProtocolVersion {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(semver::Version::new(0, 1, 0))))
    }
}

impl SharedProtocolVersion {
    pub fn replace(&self, version: semver::Version) {
        *self.0.lock().unwrap() = version;
    }

    pub fn matches(&self, version_req: &semver::VersionReq) -> bool {
        version_req.matches(&self.0.lock().unwrap())
    }
}
