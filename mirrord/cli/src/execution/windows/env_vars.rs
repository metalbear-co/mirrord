use std::{collections::HashMap, ffi::OsString};

use str_win::string_to_u16_buffer;

#[derive(Debug, Default)]
pub struct EnvMap(pub HashMap<OsString, OsString>);

impl<K, V> From<HashMap<K, V>> for EnvMap
where
    K: Into<String>,
    V: Into<String>,
{
    fn from(input: HashMap<K, V>) -> Self {
        let mut result = HashMap::<OsString, OsString>::with_capacity(input.len());

        for (k, v) in input {
            result.insert(OsString::from(k.into()), OsString::from(v.into()));
        }
        EnvMap(result)
    }
}

// pub trait EnvExt {
//     fn to_windows_env_block(&self) -> Vec<u16>;
// }
impl EnvMap {
    pub fn to_windows_env_block(&self) -> Vec<u16> {
        let mut block: Vec<u16> = Vec::new();

        for (key, val) in &self.0 {
            // Create "key=value" string and convert to UTF-16 with null terminator
            let env_entry = format!("{}={}", key.to_string_lossy(), val.to_string_lossy());
            let entry_u16 = string_to_u16_buffer(&env_entry);

            // string_to_u16_buffer already includes null terminator
            block.extend(entry_u16);
        }

        // Double-null terminate the entire block
        block.push(0);
        block
    }
}
