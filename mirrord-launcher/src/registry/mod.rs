//! Registry utilities.
//!
//! Implements a Rust-like interface to Windows Registry operations, inspired by `HashMap`.
//! Supports every value-type documented in the Windows registry.
//! Abstracts C string semantics behind regular Rust containers.
//! Automatically manages `HKEY` handles RAII.

use std::collections::HashMap;

use winapi::{
    shared::{
        minwindef::{DWORD, HKEY},
        winerror::ERROR_SUCCESS,
    },
    um::{
        errhandlingapi::{GetLastError, SetLastError},
        winnt::{REG_BINARY, REG_DWORD, REG_MULTI_SZ, REG_QWORD, REG_SZ},
        winreg::{
            HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, HKEY_USERS, RegCreateKeyW,
            RegDeleteKeyW, RegDeleteValueW, RegEnumKeyW, RegEnumValueW, RegFlushKey, RegOpenKeyW,
            RegQueryValueExW, RegSetValueExW,
        },
    },
};

use crate::handle::hkey::SafeHKey;

use mirrord_win_str::*;

/// Registry container.
///
/// Offers operations over [`HKEY`] handle, and is further responsible for
/// doing [`RegCloseKey`] through the [`SafeHKey`] class.
#[derive(Debug)]
pub struct Registry {
    key: SafeHKey,
}

#[derive(Debug, Clone)]
pub enum RegistryValue {
    /// REG_BINARY
    Binary(Vec<u8>),
    /// REG_DWORD
    Dword(u32),
    /// REG_QWORD
    Qword(u64),
    /// REG_SZ
    String(String),
    /// REG_MULTI_SZ
    MultiString(Vec<String>),
}

impl RegistryValue {
    /// Constructs [`RegistryValue`] from data, and it's type.
    ///
    /// # Arguments
    ///
    /// * `value_type` - Raw Windows variant type.
    /// * `data` - Raw data to construct [`RegistryValue`] from.
    pub fn from(value_type: u32, data: Vec<u8>) -> Option<Self> {
        match value_type {
            REG_BINARY => Some(RegistryValue::Binary(data)),
            REG_DWORD => Some(RegistryValue::Dword(unsafe {
                std::ptr::read::<u32>(data.as_ptr() as *const _)
            })),
            REG_QWORD => Some(RegistryValue::Qword(unsafe {
                std::ptr::read::<u64>(data.as_ptr() as *const _)
            })),
            REG_SZ => {
                let data = u8_data_to_u16(data)?;
                Some(RegistryValue::String(u16_buffer_to_string(data)))
            }
            REG_MULTI_SZ => {
                let data = u8_data_to_u16(data)?;
                Some(RegistryValue::MultiString(u16_multi_buffer_to_strings(
                    data,
                )))
            }
            _ => None,
        }
    }

    /// Constructs [`Vec<u8>`] from [`RegistryValue`], consuming it by move.
    pub fn to_vec(self) -> Vec<u8> {
        match self {
            RegistryValue::Binary(x) => x,
            RegistryValue::Dword(x) => u32::to_ne_bytes(x).into(),
            RegistryValue::Qword(x) => u64::to_ne_bytes(x).into(),
            RegistryValue::String(x) => u16_data_to_u8(string_to_u16_buffer(x)).unwrap_or_default(),
            RegistryValue::MultiString(x) => {
                u16_data_to_u8(strings_to_multi_string_buffer(x)).unwrap_or_default()
            }
        }
    }

    /// Maps enum variant to raw Windows variant type.
    pub fn get_type(&self) -> DWORD {
        match *self {
            RegistryValue::Binary(_) => REG_BINARY,
            RegistryValue::Dword(_) => REG_DWORD,
            RegistryValue::Qword(_) => REG_QWORD,
            RegistryValue::String(_) => REG_SZ,
            RegistryValue::MultiString(_) => REG_MULTI_SZ,
        }
    }
}

fn strings_to_multi_string_buffer<T: AsRef<[String]>>(x: T) -> Vec<u16> {
    let x = x.as_ref();

    let mut data: Vec<u16> = Vec::new();
    let strings: Vec<Vec<u16>> = x.iter().map(string_to_u16_buffer).collect();
    let strings_len = strings.len();
    for string in strings {
        for chr in string {
            data.push(chr);
        }
    }

    if strings_len > 1 {
        data.push(0);
    }

    data
}

fn u8_data_to_u16<T: AsRef<Vec<u8>>>(data: T) -> Option<Vec<u16>> {
    let data = data.as_ref();

    if (data.len() & 1) != 0 {
        return None;
    }

    Some(
        data.chunks(2)
            .map(|x| u16::from_le_bytes([x[0], x[1]]))
            .collect::<Vec<u16>>(),
    )
}

fn u16_data_to_u8<T: AsRef<Vec<u16>>>(data: T) -> Option<Vec<u8>> {
    let data = data.as_ref();

    if data.is_empty() {
        return None;
    }

    Some(data.iter().flat_map(|x| x.to_le_bytes()).collect())
}

impl Registry {
    /// Returns [`Registry`] for [`HKEY_CURRENT_USER`] hive.
    pub fn hkcu() -> Self {
        Self {
            key: SafeHKey::from(HKEY_CURRENT_USER),
        }
    }

    /// Returns [`Registry`] for [`HKEY_CLASSES_ROOT`] hive.
    pub fn hkcr() -> Self {
        Self {
            key: SafeHKey::from(HKEY_CLASSES_ROOT),
        }
    }

    /// Returns [`Registry`] for [`HKEY_USERS`] hive.
    pub fn hku() -> Self {
        Self {
            key: SafeHKey::from(HKEY_USERS),
        }
    }

    /// Returns [`Registry`] for [`HKEY_LOCAL_MACHINE`] hive.
    ///
    /// # Warning
    ///
    /// Might require Administrator privileges to do operations over it.
    pub fn hklm() -> Self {
        Self {
            key: SafeHKey::from(HKEY_LOCAL_MACHINE),
        }
    }

    /// Enumerates the keys contained within the current key,
    /// and stores them into a KV hashmap.
    pub fn keys(&self) -> HashMap<String, Self> {
        let mut index = 0;
        let mut keys: HashMap<String, Self> = HashMap::new();

        loop {
            let mut name = [0u16; 260];

            let ret = unsafe {
                RegEnumKeyW(
                    self.key.get(),
                    index,
                    name.as_mut_ptr(),
                    name.len() as u32 * 2,
                )
            } as u32;
            if ret != ERROR_SUCCESS {
                break;
            }

            let name = u16_buffer_to_string(&name[..]);
            let key = self.get_key(&name).unwrap();
            keys.insert(name, key);

            index += 1;
        }

        let error = unsafe { GetLastError() };
        if error != ERROR_SUCCESS {
            unsafe { SetLastError(ERROR_SUCCESS) };
        }

        keys
    }

    /// Enumerates the values contained within the current key,
    /// and stores them into a KV hashmap.
    pub fn values(&self) -> HashMap<String, RegistryValue> {
        let mut index = 0;
        let mut values: HashMap<String, RegistryValue> = HashMap::new();

        loop {
            // Name variables.
            let mut name = [0u16; 260];
            let mut name_len = name.len() as u32;

            // Data.
            let mut data: Vec<u8> = Vec::new();
            let mut data_len: u32 = 0;
            let mut value_type: u32 = 0;

            let ret = unsafe {
                RegEnumValueW(
                    self.key.get(),
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null_mut(),
                    &mut value_type,
                    std::ptr::null_mut(),
                    &mut data_len,
                )
            } as u32;
            if ret != ERROR_SUCCESS {
                break;
            }

            // Initialize buffer with the expected data.
            data = vec![0u8; data_len as usize];

            // Reset `name_len`, otherwise the function fails.
            name_len = name.len() as u32;

            let ret = unsafe {
                RegEnumValueW(
                    self.key.get(),
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null_mut(),
                    &mut value_type,
                    data.as_mut_ptr(),
                    &mut data_len,
                )
            } as u32;
            if ret != ERROR_SUCCESS {
                break;
            }

            let name = u16_buffer_to_string(&name[..=name_len as usize]);
            let value = RegistryValue::from(value_type, data).unwrap();
            values.insert(name, value);

            index += 1;
        }

        let error = unsafe { GetLastError() };
        if error != ERROR_SUCCESS {
            unsafe { SetLastError(ERROR_SUCCESS) };
        }

        values
    }

    /// Inserts a key into the current key. If succesful, returns
    /// [`Registry`] based on the new key.
    ///
    /// # Arguments
    ///
    /// * `key_path` - The name of the new key.
    pub fn insert_key<T: AsRef<str>>(&mut self, key_path: T) -> Option<Self> {
        let key_path = string_to_u16_buffer(key_path);
        let mut key: HKEY = std::ptr::null_mut();

        let ret = unsafe { RegCreateKeyW(self.key.get(), key_path.as_ptr(), &mut key) } as u32;
        if ret != ERROR_SUCCESS {
            return None;
        }

        Some(Self {
            key: SafeHKey::from(key),
        })
    }

    /// Retrieves a key from the current key. If succesful, returns
    /// [`Registry`] based on the retrieved key.
    ///
    /// # Arguments
    ///
    /// * `key_path` - The name of the key to be retrieved.
    pub fn get_key<T: AsRef<str>>(&self, key_path: T) -> Option<Self> {
        let key_path = string_to_u16_buffer(key_path);
        let mut key: HKEY = std::ptr::null_mut();

        let ret = unsafe { RegOpenKeyW(self.key.get(), key_path.as_ptr(), &mut key) } as u32;
        if ret != ERROR_SUCCESS {
            return None;
        }

        Some(Self {
            key: SafeHKey::from(key),
        })
    }

    /// Retrieves, or inserts, a new key from/into the current key.
    /// Returns [`Registry`] based on said key.
    ///
    /// # Arguments
    ///
    /// * `key_path` - The name of key to be inserted/retrieved.
    pub fn get_or_insert_key<T: AsRef<str>>(&mut self, key_path: T) -> Option<Self> {
        self.insert_key(key_path)
    }

    /// Deletes an existing key from the current key. Returns operation status.
    ///
    /// # Arguments
    ///
    /// * `key_path` - The name of key to be deleted.
    pub fn delete_key<T: AsRef<str>>(&mut self, key_path: T) -> bool {
        let key_path = string_to_u16_buffer(key_path);

        let ret = unsafe { RegDeleteKeyW(self.key.get(), key_path.as_ptr()) } as u32;
        if ret != ERROR_SUCCESS {
            return false;
        }

        true
    }

    /// Retrieves a value from the current key. If succesful, returns
    /// [`RegistryValue`].
    ///
    /// # Arguments
    ///
    /// * `value_path` - The name of the value to be retrieved.
    pub fn get_value<T: AsRef<str>>(&self, value_path: T) -> Option<RegistryValue> {
        let value_path = string_to_u16_buffer(value_path);

        // Call once to get the type of the data, and the size, in bytes.
        let mut value_type = 0u32;
        let mut data_bytes = 0u32;
        let ret = unsafe {
            RegQueryValueExW(
                self.key.get(),
                value_path.as_ptr(),
                std::ptr::null_mut(),
                &mut value_type,
                std::ptr::null_mut(),
                &mut data_bytes,
            )
        } as u32;

        if ret != ERROR_SUCCESS {
            return None;
        }

        // Call again to get the data, in bytes, knowing the size.
        let mut data = vec![0u8; data_bytes as usize];
        let ret = unsafe {
            RegQueryValueExW(
                self.key.get(),
                value_path.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                data.as_mut_ptr(),
                &mut data_bytes,
            )
        } as u32;

        if ret != ERROR_SUCCESS {
            return None;
        }

        RegistryValue::from(value_type, data)
    }

    pub fn get_value_binary<T: AsRef<str>>(&self, value_path: T) -> Option<Vec<u8>> {
        let value = self.get_value(value_path)?;
        match value {
            RegistryValue::Binary(x) => Some(x),
            _ => None,
        }
    }

    pub fn get_value_dword<T: AsRef<str>>(&self, value_path: T) -> Option<u32> {
        let value = self.get_value(value_path)?;
        match value {
            RegistryValue::Dword(x) => Some(x),
            _ => None,
        }
    }

    pub fn get_value_qword<T: AsRef<str>>(&self, value_path: T) -> Option<u64> {
        let value = self.get_value(value_path)?;
        match value {
            RegistryValue::Qword(x) => Some(x),
            _ => None,
        }
    }

    pub fn get_value_string<T: AsRef<str>>(&self, value_path: T) -> Option<String> {
        let value = self.get_value(value_path)?;
        match value {
            RegistryValue::String(x) => Some(x),
            _ => None,
        }
    }

    pub fn get_value_multi_string<T: AsRef<str>>(&self, value_path: T) -> Option<Vec<String>> {
        let value = self.get_value(value_path)?;
        match value {
            RegistryValue::MultiString(x) => Some(x),
            _ => None,
        }
    }

    /// Inserts a value into the current key. Returns operation status.
    ///
    /// # Arguments
    ///
    /// * `value_path` - The name of value to be inserted.
    /// * `value` - The value to be inserted.
    pub fn insert_value<T: AsRef<str>>(&mut self, value_path: T, value: RegistryValue) -> bool {
        let value_path = string_to_u16_buffer(value_path);

        // Get data type, buffer and length.
        let value_type = value.get_type();
        let data = value.to_vec();
        let data_len = data.len() as u32;

        if data.is_empty() {
            return false;
        }

        let ret = unsafe {
            RegSetValueExW(
                self.key.get(),
                value_path.as_ptr(),
                0,
                value_type,
                data.as_ptr(),
                data_len,
            )
        } as u32;

        if ret != ERROR_SUCCESS {
            return false;
        }

        true
    }

    pub fn insert_value_binary<T: AsRef<str>>(&mut self, value_path: T, value: Vec<u8>) -> bool {
        let value = RegistryValue::Binary(value);
        self.insert_value(value_path, value)
    }

    pub fn insert_value_dword<T: AsRef<str>>(&mut self, value_path: T, value: u32) -> bool {
        let value = RegistryValue::Dword(value);
        self.insert_value(value_path, value)
    }

    pub fn insert_value_qword<T: AsRef<str>>(&mut self, value_path: T, value: u64) -> bool {
        let value = RegistryValue::Qword(value);
        self.insert_value(value_path, value)
    }

    pub fn insert_value_string<T: AsRef<str>>(&mut self, value_path: T, value: String) -> bool {
        let value = RegistryValue::String(value);
        self.insert_value(value_path, value)
    }

    pub fn insert_value_multi_string<T: AsRef<str>>(
        &mut self,
        value_path: T,
        value: Vec<String>,
    ) -> bool {
        let value = RegistryValue::MultiString(value);
        self.insert_value(value_path, value)
    }

    /// Deletes an existing value from the current key. Returns operation status.
    ///
    /// # Arguments
    ///
    /// * `value_path` - The name of value to be deleted.
    pub fn delete_value<T: AsRef<str>>(&mut self, value_path: T) -> bool {
        let value_path = string_to_u16_buffer(value_path);

        let ret = unsafe { RegDeleteValueW(self.key.get(), value_path.as_ptr()) } as u32;
        if ret != ERROR_SUCCESS {
            return false;
        }

        true
    }

    /// Flushes changes made towards the current key to disk.
    ///
    /// # MSDN
    ///
    /// `Calling RegFlushKey is an expensive operation that significantly affects
    /// system-wide performance as it consumes disk bandwidth and blocks modifications
    /// to all keys by all processes in the registry hive that is being flushed until the flush
    /// operation completes. RegFlushKey should only be called explicitly when an application
    /// must guarantee that registry changes are persisted to disk immediately after
    /// modification. All modifications made to keys are visible to other processes without
    /// the need to flush them to disk.`
    ///
    /// `The RegFlushKey function returns only when all the data for the hive that contains
    /// the specified key has been written to the registry store on disk.`
    ///
    /// `The RegFlushKey function writes out the data for other keys in the hive that have
    /// been modified since the last lazy flush or system start.`
    pub fn flush(&mut self) -> bool {
        let ret = unsafe { RegFlushKey(self.key.get()) } as u32;
        if ret != ERROR_SUCCESS {
            return false;
        }

        true
    }
}

#[cfg(test)]
mod tests;
