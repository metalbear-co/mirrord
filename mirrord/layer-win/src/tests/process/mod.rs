use crate::process::{get_export, get_module_base, get_module_from_address, get_module_name};

pub mod memory;

#[test]
fn get_module_from_address_in_kernel32() {
    let create_file = get_export("ntdll", "NtCreateFile");

    let ntdll = get_module_base("ntdll");
    let ntdll_from_address = get_module_from_address(create_file);

    assert_ne!(ntdll, std::ptr::null_mut());
    assert_ne!(ntdll_from_address, std::ptr::null_mut());
    assert_eq!(ntdll, ntdll_from_address);
}

#[test]
fn get_module_name_from_address() {
    const MODULE: &str = "kernel32.dll";

    let module = get_module_base(MODULE);
    let name = get_module_name(module).map(|x| x.to_lowercase());

    assert_eq!(name, Some(MODULE.to_string()));
}
