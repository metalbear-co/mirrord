use windows::core::{PCWSTR, PWSTR};

pub struct OwnedWSTR {
    container: Vec<u16>,
}

impl From<&str> for OwnedWSTR {
    fn from(val: &str) -> Self {
        OwnedWSTR {
            // ------------------------- .chain(null terminator)
            container: val.encode_utf16().chain([0u16]).collect(),
        }
    }
}

impl From<&String> for OwnedWSTR {
    fn from(val: &String) -> Self {
        OwnedWSTR::from(val.as_str())
    }
}

// For read-only (immutable) reference
impl From<&OwnedWSTR> for PCWSTR {
    fn from(value: &OwnedWSTR) -> Self {
        PCWSTR::from_raw(value.container.as_ptr())
    }
}

// For mutable reference
impl From<&mut OwnedWSTR> for PWSTR {
    fn from(value: &mut OwnedWSTR) -> Self {
        PWSTR::from_raw(value.container.as_mut_ptr())
    }
}
