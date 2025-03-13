use std::{collections::HashMap, ffi::OsString};

pub struct ContainerEnvFile {
    envs: HashMap<OsString, OsString>,
}

