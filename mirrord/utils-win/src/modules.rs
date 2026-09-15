//! Loaded-module helpers.
//!
//! This module owns two things. First are the small relocated helpers that resolve a module by
//! name or address. Second is a cached [`ModuleTable`] plus the security-vendor flagging used by
//! the crash diagnostics.
//!
//! The [`ModuleTable`] is the loader-free piece. It captures a sorted snapshot at a safe time. The
//! crash handler can then map a faulting address to a module without any loader call.

use std::{
    ffi::c_void,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use str_win::{string_to_u16_buffer, u16_buffer_to_string};
use winapi::{
    shared::{
        minwindef::{DWORD, FALSE, HMODULE, LPVOID},
        ntdef::HANDLE,
    },
    um::{
        libloaderapi::{
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GetModuleFileNameW, GetModuleHandleExW,
            GetModuleHandleW,
        },
        processthreadsapi::GetCurrentProcess,
        psapi::{
            EnumProcessModulesEx, GetModuleFileNameExW, GetModuleInformation, LIST_MODULES_ALL,
            MODULEINFO,
        },
        winver::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW},
    },
};

/// Wide-buffer length for module path reads. Long paths beyond this are truncated.
const PATH_BUFFER_LEN: usize = 1024;

/// Obtains the base address of a module. `NULL` is returned if not found.
///
/// # Arguments
///
/// * `module` - Name of the module to retrieve base address for.
pub fn get_module_base<T: AsRef<str>>(module: T) -> *mut c_void {
    let module = string_to_u16_buffer(module);

    let base_address = unsafe { GetModuleHandleW(module.as_ptr()) };
    base_address as _
}

/// Obtains the base address of a module, from an address inside that module.
///
/// `NULL` is returned if not found.
///
/// # Arguments
///
/// * `ptr` - Address that may or may not be inside a module in the process module list.
pub fn get_module_from_address(ptr: *mut c_void) -> *mut c_void {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }

    let mut result: *mut c_void = std::ptr::null_mut();

    let ret = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            ptr as _,
            std::ptr::from_mut(&mut result) as _,
        )
    };
    if ret == 0 {
        std::ptr::null_mut()
    } else {
        result as _
    }
}

/// Obtains the name of a module from its base address. `None` if it doesn't have one.
///
/// # Arguments
///
/// * `module` - Address that may or may not point to a module.
pub fn get_module_name(module: *mut c_void) -> Option<String> {
    if module.is_null() {
        return None;
    }

    let mut name = [0u16; 512];

    let ret = unsafe { GetModuleFileNameW(module as _, name.as_mut_ptr() as _, name.len() as _) };

    if ret == 0 {
        None
    } else {
        let name = u16_buffer_to_string(name);

        // The result of the operation is actually supposed to be a fully qualified path.
        let path = PathBuf::from(name);

        // Check if it's *actually* a fully qualified path.
        if !path.has_root() {
            return None;
        }

        // Try to extract file name.
        let module_name = path.file_name()?;

        Some(module_name.to_string_lossy().to_string())
    }
}

/// A module loaded into the current process.
#[derive(Debug, Clone)]
pub struct ModuleEntry {
    /// Base load address.
    pub base: usize,
    /// Image size in bytes.
    pub size: usize,
    /// File name with extension. For example `kernel32.dll`.
    pub name: String,
    /// Full path on disk.
    pub path: PathBuf,
}

/// Enumerates every module loaded into the current process.
///
/// This uses the psapi loader APIs. It allocates and touches the loader, so it must run at a safe
/// time. Never call it from a crash handler.
///
/// # Returns
///
/// One [`ModuleEntry`] per loaded module, in no particular order. An empty vector on failure.
pub fn enumerate_modules() -> Vec<ModuleEntry> {
    enumerate_modules_of(unsafe { GetCurrentProcess() })
}

/// Enumerates every module loaded into the given process.
///
/// Same loader-touching caveat as [`enumerate_modules`]. The monitor reaches it through
/// [`module_inventory`] to inventory the frozen crashing process from the outside.
///
/// # Arguments
///
/// * `process` - an open handle carrying `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ`.
///
/// # Returns
///
/// One [`ModuleEntry`] per loaded module, in no particular order. An empty vector on failure.
fn enumerate_modules_of(process: HANDLE) -> Vec<ModuleEntry> {
    // Query, then re-query once if the module list grew between calls.
    let mut handles: Vec<HMODULE> = Vec::new();
    let mut capacity = 256usize;
    loop {
        handles.resize(capacity, std::ptr::null_mut());
        let cb = (capacity * std::mem::size_of::<HMODULE>()) as DWORD;
        let mut needed: DWORD = 0;
        let ok = unsafe {
            EnumProcessModulesEx(
                process,
                handles.as_mut_ptr(),
                cb,
                &mut needed,
                LIST_MODULES_ALL,
            )
        };
        if ok == FALSE {
            return Vec::new();
        }

        let needed_count = needed as usize / std::mem::size_of::<HMODULE>();
        if needed_count <= capacity {
            handles.truncate(needed_count);
            break;
        }
        capacity = needed_count;
    }

    handles
        .into_iter()
        .filter_map(|module| module_entry(process, module))
        .collect()
}

/// Reads one module's base, size, and path.
fn module_entry(process: HANDLE, module: HMODULE) -> Option<ModuleEntry> {
    let mut info: MODULEINFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetModuleInformation(
            process,
            module,
            &mut info,
            std::mem::size_of::<MODULEINFO>() as DWORD,
        )
    };
    if ok == FALSE {
        return None;
    }

    let mut buffer = [0u16; PATH_BUFFER_LEN];
    let len = unsafe {
        GetModuleFileNameExW(process, module, buffer.as_mut_ptr(), buffer.len() as DWORD)
    };
    if len == 0 {
        return None;
    }

    let written = buffer.get(..len as usize).unwrap_or(&buffer);
    let path = PathBuf::from(u16_buffer_to_string(written));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();

    Some(ModuleEntry {
        base: info.lpBaseOfDll as usize,
        size: info.SizeOfImage as usize,
        name,
        path,
    })
}

/// Builds a human-readable module inventory of a process for the crash bundle.
///
/// Lists every loaded module with its base address, image size, and path. Vendor flagging is left
/// to the startup snapshot in the log, so this stays fast enough to run while the crashing process
/// is frozen on the dump handshake.
///
/// # Arguments
///
/// * `process` - an open handle carrying `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ`.
///
/// # Returns
///
/// A multi-line inventory, or an empty string when enumeration fails.
pub fn module_inventory(process: HANDLE) -> String {
    let mut entries = enumerate_modules_of(process);
    entries.sort_by_key(|entry| entry.base);

    let mut out = String::new();
    let _ = writeln!(out, "modules loaded: {}", entries.len());
    let _ = writeln!(out);

    for entry in &entries {
        let _ = writeln!(
            out,
            "{:#018x}  {:>12}  {}",
            entry.base,
            entry.size,
            entry.path.display(),
        );
    }
    out
}

/// A sorted snapshot of the loaded modules.
///
/// It maps an address back to its owning module by binary search. It holds no handles and makes no
/// loader calls. So the crash handler can use it after a fault, when the loader lock may be held.
#[derive(Debug, Clone, Default)]
pub struct ModuleTable {
    /// Entries sorted by base address.
    entries: Vec<ModuleEntry>,
}

impl ModuleTable {
    /// Captures the current module list into a sorted table.
    ///
    /// Run this at a safe time. The capture itself touches the loader.
    pub fn capture() -> Self {
        let mut entries = enumerate_modules();
        entries.sort_by_key(|entry| entry.base);
        Self { entries }
    }

    /// Resolves an address to the name of its owning module.
    ///
    /// This is a pure binary search. It is safe to call from a crash handler.
    ///
    /// # Arguments
    ///
    /// * `address` - the address to resolve.
    ///
    /// # Returns
    ///
    /// The owning module's file name, or `None` when the address is in no known module.
    pub fn resolve(&self, address: usize) -> Option<&str> {
        self.resolve_offset(address).map(|(name, _)| name)
    }

    /// Resolves an address to its owning module name and the offset into that module.
    ///
    /// This is a pure binary search. It is safe to call from a crash handler.
    ///
    /// # Arguments
    ///
    /// * `address` - the address to resolve.
    ///
    /// # Returns
    ///
    /// The owning module's file name and the byte offset from its base, or `None` when the address
    /// is in no known module.
    pub fn resolve_offset(&self, address: usize) -> Option<(&str, usize)> {
        // The count of entries whose base is at or below the address.
        let upper = self.entries.partition_point(|entry| entry.base <= address);
        let entry = self.entries.get(upper.checked_sub(1)?)?;
        if address < entry.base.saturating_add(entry.size) {
            Some((entry.name.as_str(), address - entry.base))
        } else {
            None
        }
    }

    /// Returns the captured entries, sorted by base address.
    pub fn entries(&self) -> &[ModuleEntry] {
        &self.entries
    }

    /// Returns the number of captured modules.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Reads the `CompanyName` from a module's version resource.
///
/// The lookup walks the version block. It picks the first language translation. Then it reads that
/// translation's `CompanyName` string.
///
/// # Arguments
///
/// * `path` - the module's full path on disk.
///
/// # Returns
///
/// The company name, or `None` when the file has no version resource or no company string.
pub fn module_company(path: &Path) -> Option<String> {
    let wide = string_to_u16_buffer(path.to_string_lossy());

    let mut handle: DWORD = 0;
    let size = unsafe { GetFileVersionInfoSizeW(wide.as_ptr(), &mut handle) };
    if size == 0 {
        return None;
    }

    let mut block = vec![0u8; size as usize];
    let ok = unsafe { GetFileVersionInfoW(wide.as_ptr(), 0, size, block.as_mut_ptr() as *mut _) };
    if ok == FALSE {
        return None;
    }

    let (language, codepage) = query_translation(&block)?;
    let sub_block = format!("\\StringFileInfo\\{language:04x}{codepage:04x}\\CompanyName");
    query_version_string(&block, &sub_block)
}

/// Reads the first language and codepage pair from a version block.
fn query_translation(block: &[u8]) -> Option<(u16, u16)> {
    let key = string_to_u16_buffer("\\VarFileInfo\\Translation");
    let mut buffer: LPVOID = std::ptr::null_mut();
    let mut len: u32 = 0;
    let ok = unsafe {
        VerQueryValueW(
            block.as_ptr() as *const _,
            key.as_ptr(),
            &mut buffer,
            &mut len,
        )
    };
    if ok == FALSE || buffer.is_null() || len < 4 {
        return None;
    }

    // The value is an array of [language, codepage] u16 pairs. Take the first.
    let language = unsafe { *(buffer as *const u16) };
    let codepage = unsafe { *(buffer as *const u16).add(1) };
    Some((language, codepage))
}

/// Reads a string value out of a version block.
fn query_version_string(block: &[u8], sub_block: &str) -> Option<String> {
    let key = string_to_u16_buffer(sub_block);
    let mut buffer: LPVOID = std::ptr::null_mut();
    let mut len: u32 = 0;
    let ok = unsafe {
        VerQueryValueW(
            block.as_ptr() as *const _,
            key.as_ptr(),
            &mut buffer,
            &mut len,
        )
    };
    if ok == FALSE || buffer.is_null() || len == 0 {
        return None;
    }

    // `len` counts characters including the terminating null.
    let chars = unsafe { std::slice::from_raw_parts(buffer as *const u16, len as usize) };
    let value = u16_buffer_to_string(chars);
    if value.is_empty() { None } else { Some(value) }
}

/// A security or endpoint-protection vendor we recognize.
pub struct SecurityVendor {
    /// Display name. For example `CrowdStrike`.
    pub name: &'static str,
    /// Lowercase needles matched against a module's file name and `CompanyName`.
    pub needles: &'static [&'static str],
}

/// Known security and endpoint-protection products.
///
/// Each entry's needles are matched against the module file name and its `CompanyName`. A hit on
/// either is enough. The needles are deliberately specific. Bare vendor words that collide with
/// unrelated software are avoided. For Microsoft Defender the company is too generic to use. So it
/// is matched on its in-process file names only.
pub static SECURITY_VENDORS: &[SecurityVendor] = &[
    SecurityVendor {
        name: "CrowdStrike",
        needles: &["crowdstrike", "csagent", "csfalcon"],
    },
    SecurityVendor {
        name: "SentinelOne",
        needles: &["sentinelone", "sentinel labs", "inprocessclient"],
    },
    SecurityVendor {
        name: "Carbon Black",
        needles: &["carbon black", "cb defense", "cbdefense", "umppc", "cbk7"],
    },
    SecurityVendor {
        name: "Cylance",
        needles: &["cylance", "cymemdef"],
    },
    SecurityVendor {
        name: "Microsoft Defender",
        needles: &["windows defender", "mpoav", "mpclient"],
    },
    SecurityVendor {
        name: "Cisco Secure Endpoint",
        needles: &["cisco amp", "secure endpoint", "immunet"],
    },
    SecurityVendor {
        name: "Sophos",
        needles: &["sophos", "hitmanpro"],
    },
    SecurityVendor {
        name: "McAfee / Trellix",
        needles: &["mcafee", "trellix", "mfehc"],
    },
    SecurityVendor {
        name: "Symantec",
        needles: &["symantec", "sysfer", "norton"],
    },
    SecurityVendor {
        name: "Trend Micro",
        needles: &["trend micro", "tmmon", "tmumh"],
    },
    SecurityVendor {
        name: "ESET",
        needles: &["eset", "eamsi"],
    },
    SecurityVendor {
        name: "Bitdefender",
        needles: &["bitdefender", "atcuf", "bdhkm"],
    },
    SecurityVendor {
        name: "Kaspersky",
        needles: &["kaspersky", "klsihk", "kfltadk"],
    },
    SecurityVendor {
        name: "Palo Alto Cortex",
        needles: &["palo alto", "cortex", "cyvera"],
    },
    SecurityVendor {
        name: "Elastic Endgame",
        needles: &["endgame", "elastic endpoint"],
    },
    SecurityVendor {
        name: "FireEye / Mandiant",
        needles: &["fireeye", "mandiant", "xagt"],
    },
    SecurityVendor {
        name: "Webroot",
        needles: &["webroot", "wrsa"],
    },
    SecurityVendor {
        name: "Malwarebytes",
        needles: &["malwarebytes", "mbae", "mbam"],
    },
    SecurityVendor {
        name: "Check Point",
        needles: &["check point", "checkpoint"],
    },
    SecurityVendor {
        name: "Fortinet",
        needles: &["fortinet", "forticlient"],
    },
    SecurityVendor {
        name: "Deep Instinct",
        needles: &["deep instinct"],
    },
    SecurityVendor {
        name: "BeyondTrust / Avecto",
        needles: &["beyondtrust", "avecto", "defendpoint", "pghook"],
    },
];

/// A loaded module attributed to a known security vendor.
#[derive(Debug, Clone)]
pub struct FlaggedModule {
    /// Module file name. For example `csagent.sys`.
    pub name: String,
    /// The recognized vendor display name.
    pub vendor: &'static str,
}

/// Scans the loaded modules for known security and endpoint-protection products.
///
/// This captures a fresh module table. Use [`flag_security_modules`] when a table already exists.
///
/// # Returns
///
/// One [`FlaggedModule`] per recognized module, in load-address order.
pub fn flagged_security_modules() -> Vec<FlaggedModule> {
    flag_security_modules(&ModuleTable::capture())
}

/// Scans a captured [`ModuleTable`] for known security products.
///
/// Each module's file name and `CompanyName` are matched against [`SECURITY_VENDORS`]. A hit on
/// either is enough. Reading the company name touches the file. So this is not safe for a crash
/// handler.
///
/// # Arguments
///
/// * `table` - the module snapshot to scan.
///
/// # Returns
///
/// One [`FlaggedModule`] per recognized module, in the table's order.
pub fn flag_security_modules(table: &ModuleTable) -> Vec<FlaggedModule> {
    table
        .entries()
        .iter()
        .filter_map(|entry| {
            let company = module_company(&entry.path);
            match_vendor(&entry.name, company.as_deref()).map(|vendor| FlaggedModule {
                name: entry.name.clone(),
                vendor,
            })
        })
        .collect()
}

/// Matches a module name and optional company against [`SECURITY_VENDORS`].
fn match_vendor(name: &str, company: Option<&str>) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    let company = company.map(str::to_ascii_lowercase);
    SECURITY_VENDORS.iter().find_map(|vendor| {
        let hit = vendor.needles.iter().any(|needle| {
            name.contains(needle)
                || company
                    .as_deref()
                    .is_some_and(|company| company.contains(needle))
        });
        hit.then_some(vendor.name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(base: usize, size: usize, name: &str) -> ModuleEntry {
        ModuleEntry {
            base,
            size,
            name: name.to_owned(),
            path: PathBuf::from(name),
        }
    }

    /// Builds a table directly from entries, sorted by base as [`ModuleTable::capture`] leaves it.
    fn table(mut entries: Vec<ModuleEntry>) -> ModuleTable {
        entries.sort_by_key(|entry| entry.base);
        ModuleTable { entries }
    }

    #[test]
    fn resolve_offset_empty_table() {
        assert_eq!(table(vec![]).resolve_offset(0x1000), None);
    }

    #[test]
    fn resolve_offset_below_all_bases() {
        let modules = table(vec![entry(0x2000, 0x1000, "a.dll")]);
        assert_eq!(modules.resolve_offset(0x1000), None);
    }

    #[test]
    fn resolve_offset_inside_and_at_base() {
        let modules = table(vec![
            entry(0x1000, 0x1000, "a.dll"),
            entry(0x4000, 0x2000, "b.dll"),
        ]);
        // At the base: offset 0.
        assert_eq!(modules.resolve_offset(0x1000), Some(("a.dll", 0)));
        // Inside the first module.
        assert_eq!(modules.resolve_offset(0x1500), Some(("a.dll", 0x500)));
        // Inside the second module.
        assert_eq!(modules.resolve_offset(0x4001), Some(("b.dll", 1)));
    }

    #[test]
    fn resolve_offset_in_a_gap_between_modules() {
        let modules = table(vec![
            entry(0x1000, 0x1000, "a.dll"),
            entry(0x4000, 0x1000, "b.dll"),
        ]);
        // 0x2500 is past a.dll's end (0x2000) and before b.dll's base (0x4000).
        assert_eq!(modules.resolve_offset(0x2500), None);
        // Exactly at a.dll's end is one past the last valid byte.
        assert_eq!(modules.resolve_offset(0x2000), None);
        // The last module's tail byte resolves; one past it does not.
        assert_eq!(modules.resolve_offset(0x4FFF), Some(("b.dll", 0xFFF)));
        assert_eq!(modules.resolve_offset(0x5000), None);
    }

    #[test]
    fn match_vendor_by_file_name() {
        assert_eq!(match_vendor("csagent.sys", None), Some("CrowdStrike"));
        // Microsoft Defender is matched on its file name; its company is deliberately omitted.
        assert_eq!(
            match_vendor("MpClient.dll", None),
            Some("Microsoft Defender")
        );
    }

    #[test]
    fn match_vendor_is_case_insensitive() {
        assert_eq!(match_vendor("CSAgent.SYS", None), Some("CrowdStrike"));
    }

    #[test]
    fn match_vendor_by_company() {
        assert_eq!(
            match_vendor("driver.sys", Some("Sophos Ltd")),
            Some("Sophos")
        );
    }

    #[test]
    fn match_vendor_no_hit() {
        assert_eq!(match_vendor("kernel32.dll", None), None);
    }
}
