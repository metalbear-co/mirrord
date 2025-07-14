use windows::core::{PCWSTR, PWSTR};

pub struct OwnedPCWSTR {
    container: Vec<u16>,
}

impl OwnedPCWSTR {
    pub fn from_string(val: &String) -> Self {
        OwnedPCWSTR {
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

pub fn build(commandline: Vec<String>) -> Result<(OwnedPCWSTR, OwnedPCWSTR), ()> {
    if commandline.len() < 1 {
        return Err(());
    }

    let ([applicationname], commandline) = commandline.split_at(1) else {
        return Err(());
    };
    let commandline = commandline.join(" ");

    let applicationname = OwnedPCWSTR::from_string(applicationname);
    let commandline = OwnedPCWSTR::from_string(&commandline);
    Ok((applicationname, commandline))
}
