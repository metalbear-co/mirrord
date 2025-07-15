#![cfg(target_arch = "x86_64")]

mod dll;

use minhook::MinHook;
use mirrord_win_str::u16_buffer_to_string;
use winapi::{
    ctypes::c_void,
    shared::{
        minwindef::{BOOL, DWORD, LPCVOID, LPDWORD, LPVOID},
        ntdef::HANDLE,
    },
    um::{
        consoleapi::AllocConsole,
        fileapi::GetFinalPathNameByHandleW,
        libloaderapi::{GetModuleHandleA, GetProcAddress},
        minwinbase::LPOVERLAPPED,
        winnt::DLL_PROCESS_ATTACH,
    },
};

/// Whether [`AllocConsole`] should be called. Useful for debugging.
const CONSOLE: bool = false;

fn get_file_name_by_handle(handle: HANDLE) -> String {
    let mut name = [0u16; 260];
    let name_len =
        unsafe { GetFinalPathNameByHandleW(handle, name.as_mut_ptr(), name.len() as u32, 0) };

    u16_buffer_to_string(&name[..=name_len as usize])
}

type ReadFileHook = unsafe extern "stdcall" fn(HANDLE, LPVOID, DWORD, LPDWORD, LPOVERLAPPED);
unsafe extern "stdcall" fn read_file_hook(
    file: HANDLE,
    buffer: LPVOID,
    number_of_bytes_to_read: DWORD,
    number_of_bytes_read: LPDWORD,
    overlapped: LPOVERLAPPED,
) {
    unsafe {
        let file_name = get_file_name_by_handle(file);
        println!("read_file_hook: {file_name}");

        let original = READ_FILE_ORIGINAL.unwrap();
        original(
            file,
            buffer,
            number_of_bytes_to_read,
            number_of_bytes_read,
            overlapped,
        )
    }
}
static mut READ_FILE_ORIGINAL: Option<ReadFileHook> = None;

type WriteFileHook =
    unsafe extern "stdcall" fn(HANDLE, LPCVOID, DWORD, LPDWORD, LPOVERLAPPED) -> BOOL;
unsafe extern "stdcall" fn write_file_hook(
    file: HANDLE,
    buffer: LPCVOID,
    number_of_bytes_to_write: DWORD,
    number_of_bytes_written: LPDWORD,
    overlapped: LPOVERLAPPED,
) -> BOOL {
    unsafe {
        let file_name = get_file_name_by_handle(file);
        println!("write_file_hook: {file_name}");

        let original = WRITE_FILE_ORIGINAL.unwrap();
        original(
            file,
            buffer,
            number_of_bytes_to_write,
            number_of_bytes_written,
            overlapped,
        )
    }
}
static mut WRITE_FILE_ORIGINAL: Option<WriteFileHook> = None;

entry_point!(|_, reason_for_call| {
    if reason_for_call != DLL_PROCESS_ATTACH {
        return false;
    }

    if CONSOLE {
        unsafe { AllocConsole() };
    }

    unsafe {
        let kernelbase = GetModuleHandleA(b"kernelbase\0".as_ptr() as _);
        let read_file = GetProcAddress(kernelbase, b"ReadFile\0".as_ptr() as _);
        let write_file = GetProcAddress(kernelbase, b"WriteFile\0".as_ptr() as _);

        let read_file_original = MinHook::create_hook(read_file as _, read_file_hook as _)
            .expect("Failed ReadFile hook");
        READ_FILE_ORIGINAL = Some(std::mem::transmute::<*mut c_void, ReadFileHook>(
            read_file_original as _,
        ));

        let write_file_original = MinHook::create_hook(write_file as _, write_file_hook as _)
            .expect("Failed WriteFile hook");
        WRITE_FILE_ORIGINAL = Some(std::mem::transmute::<*mut c_void, WriteFileHook>(
            write_file_original as _,
        ));

        MinHook::enable_all_hooks().expect("Failed enabling hooks");
    }

    true
});
