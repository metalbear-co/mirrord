use windows::core::{PCWSTR, PWSTR};

pub struct OwnedWSTR {
    container: Vec<u16>,
}

impl OwnedWSTR {
    pub fn from_string(val: &String) -> Self {
        OwnedWSTR {
            // chain(null terminator) -----------VVVV
            container: val.encode_utf16().chain([0u16]).collect(),
        }
    }
    pub fn pcwstr(&self) -> PCWSTR {
        PCWSTR::from_raw(self.container.as_ptr())
    }

    pub fn pwstr(&mut self) -> PWSTR {
        PWSTR::from_raw(self.container.as_mut_ptr())
    }
}
