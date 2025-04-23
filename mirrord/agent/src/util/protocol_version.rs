use std::sync::{Arc, Mutex};

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
