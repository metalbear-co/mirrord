#![allow(non_camel_case_types)]

use crate::error::HookError;

#[repr(C, packed(4))]
pub struct dns_sortaddr_t {
    pub address: libc::in_addr,
    pub mask: libc::in_addr,
}

#[repr(C, packed(4))]
pub struct dns_resolver_t {
    pub domain: *mut libc::c_char,
    pub n_nameserver: i32,
    pub nameserver: *mut *mut libc::sockaddr,
    pub port: u16,
    pub n_search: i32,
    pub search: *mut *mut libc::c_char,
    pub n_sortaddr: i32,
    pub sortaddr: *mut *mut dns_sortaddr_t,
    pub options: *mut libc::c_char,
    pub timeout: u32,
    pub search_order: u32,
    pub if_index: u32,
    pub flags: u32,
    pub reach_flags: u32,
    pub reserved: [u32; 5],
}

#[repr(C, packed(4))]
pub struct dns_config_t {
    pub n_resolver: i32,
    pub resolver: *mut *mut dns_resolver_t,
    pub n_scoped_resolver: i32,
    pub scoped_resolver: *mut *mut dns_resolver_t,
    pub reserved: [u32; 5],
}

impl From<HookError> for *mut dns_config_t {
    fn from(fail: HookError) -> Self {
        let _ = i64::from(fail);

        std::ptr::null_mut()
    }
}
