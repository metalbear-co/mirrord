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

impl EnvMap {
    pub fn to_windows_env_block(&self) -> Vec<u16> {
        let mut entries: Vec<(String, String, String)> = self
            .0
            .iter()
            .map(|(key, val)| {
                let key_string = key.to_string_lossy().into_owned();
                let lower_key = key_string.to_lowercase();
                let value_string = val.to_string_lossy().into_owned();
                (lower_key, key_string, value_string)
            })
            .collect();

        // The Windows environment block is expected to be sorted case-insensitively.
        entries.sort_by(|(lower_a, key_a, _), (lower_b, key_b, _)| {
            lower_a.cmp(lower_b).then_with(|| key_a.cmp(key_b))
        });

        let mut block: Vec<u16> = Vec::new();

        for (_, key, value) in entries {
            let env_entry = format!("{key}={value}");
            let entry_u16 = string_to_u16_buffer(&env_entry);
            block.extend(entry_u16);
        }

        if block.is_empty() {
            block.push(0);
        }
        block.push(0);

        block
    }
}
