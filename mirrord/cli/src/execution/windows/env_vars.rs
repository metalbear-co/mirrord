use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;
use std::collections::HashMap;

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
            let key: &OsStr = key.as_ref();
            let mut entry: Vec<u16> = key.encode_wide().collect();
            entry.push('=' as u16);
            // if let Some(val) = val {
            entry.extend(val.as_os_str().encode_wide());
            // }
            entry.push(0); // null-terminate each entry

            block.extend(entry);
        }

        block.push(0); // double-null terminate the entire block
        block
    }
}