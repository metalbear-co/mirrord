/// Logic for injecting our environment variables to Go program
/// Go `main` function doesn't do the libc flow, so any environment change we do using
/// standard library which uses libc will not be reflected in the Go program.
/// In order to overcome that, we hook the Go function that initializes the environment
/// variables. The function that does that is `runtime.goenvs_unix` - it does that by
/// accesssing `argc`/`argv` globals then setting the global `environ` variable that contains
/// the envs. When we get called from the detour, we replace Go's argv with our own and then
/// call the original function. This way, the Go program will see our environment variables.
/// P.S - environment variables at the end of argv, specifically `argv[argc+1]`.
use std::{ffi::CString, mem::ManuallyDrop};

use frida_gum::interceptor::Interceptor;
use libc::c_char;
use mirrord_macro::hook_fn;

use crate::replace_symbol;

/// Formats an argv as Go (and system) expects it to be which is an array of pointers
/// to C null terminated strings. The array contains null pointer at the end and as a marker
/// for dividing the array between arguments and environment variables (arg1, arg2, null, env1=val1,
/// env2=val2, null) This leaks memory on purpose (should live aslong as the app lives)
fn make_argv() -> Vec<*mut c_char> {
    let size = std::env::args().len();
    // Add arguments to our new argv
    let mut argv = Vec::with_capacity(size);
    for arg in std::env::args() {
        argv.push(CString::new(arg).unwrap().into_raw());
    }
    argv.push(std::ptr::null_mut());

    // Add environment variables to our new argv
    for (key, value) in std::env::vars() {
        argv.push(CString::new(format!("{key}={value}")).unwrap().into_raw());
    }

    argv.push(std::ptr::null_mut());
    argv
}

#[hook_fn]
unsafe extern "C" fn goenvs_unix_detour() {
    stacker::grow(32 * 1024 * 1024, || {
        let modules = frida_gum::Module::enumerate_modules();
        let binary = &modules.first().unwrap().name;
        if let Some(argv) = frida_gum::Module::find_symbol_by_name(binary, "runtime.argv") {
            let mut new_argv = ManuallyDrop::new(make_argv());
            let argv_ptr: *mut *mut *mut i8 = argv.0.cast();
            std::ptr::replace(argv_ptr, new_argv.as_mut_ptr());
        }
    });
    FN_GOENVS_UNIX();
}

pub(crate) fn enable_go_env(interceptor: &mut Interceptor, binary: &str) {
    unsafe {
        let _ = replace_symbol!(
            interceptor,
            "runtime.goenvs_unix",
            goenvs_unix_detour,
            FnGoenvs_unix,
            FN_GOENVS_UNIX,
            binary
        );
    }
}
