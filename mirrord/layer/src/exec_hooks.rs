#[cfg(not(target_os = "macos"))]
use std::ffi::CStr;
use std::{
    ffi::{CString, c_char},
    ptr,
};

pub(crate) mod hooks;
#[cfg(all(test, not(target_os = "macos")))]
mod tests;

/// Hold a vector of new CStrings to use instead of the original argv.
#[derive(Default, Debug, Clone)]
pub(crate) struct Argv(Vec<CString>);

impl Argv {
    /// Turns this list of [`CString`] into a C list of pointers (null-terminated).
    ///
    /// We leak the [`CString`]s, so that they may live in C-land.
    pub(crate) fn leak(self) -> *const *const c_char {
        // Leaks the strings.
        let mut list = self
            .0
            .into_iter()
            .map(|value| value.into_raw().cast_const())
            .collect::<Vec<_>>();

        // Null-terminated.
        list.push(ptr::null());

        // Leaks the list itself.
        list.into_raw_parts().0.cast_const()
    }

    #[cfg(target_os = "macos")]
    /// Convenience to [`Vec::push`] a new [`CString`].
    pub(crate) fn push(&mut self, item: CString) {
        self.0.push(item);
    }

    /// Insert or replace env variable.
    pub(crate) fn insert_env(&mut self, key: &str, value: &str) -> Result<(), std::ffi::NulError> {
        let Argv(argv) = self;
        let formatted = CString::new(format!("{key}={value}"))?;

        if let Some(value_index) = argv.iter().position(|var| {
            var.to_str()
                .map(|str_var| str_var.starts_with(&format!("{key}=")))
                .unwrap_or_default()
        }) {
            let var = argv
                .get_mut(value_index)
                .expect("argv should contain the found index");

            if formatted.count_bytes() < var.count_bytes() {
                tracing::warn!(
                    shared_sockets = ?var,
                    next_shared_sockets = ?formatted,
                    "replacing shared sockets with shorter variant"
                );
            }

            *var = formatted;
        } else {
            argv.push(formatted);
        }

        Ok(())
    }
}

impl FromIterator<CString> for Argv {
    fn from_iter<T: IntoIterator<Item = CString>>(iter: T) -> Self {
        Argv(Vec::from_iter(iter))
    }
}

/// Returns `envp` with `key` set to `value`, or [`None`] when the first entry for `key` in `envp`
/// already has `value`, or when `value` has a NUL byte.
///
/// The new entry takes the place of the first entry for `key`, and the other entries for `key` are
/// dropped, so the new image can't read a stale value. When `envp` has no entry for `key`, the new
/// entry goes at the end.
///
/// The result is leaked: `execve` reads it after the caller returns, and a successful `exec` never
/// returns to free it. In glibc's `posix_spawn`, `execve` runs in a `vfork` child that shares the
/// parent's memory, so the leak stays in the parent, once per spawn. To keep it small, the result
/// points to the strings of `envp`, and only the pointer list and the new entry are allocated.
///
/// # Safety
///
/// `envp` must be null, or a null-terminated array of C strings that stay valid while the result
/// is used.
#[cfg(not(target_os = "macos"))]
pub(crate) unsafe fn with_env(
    envp: *const *const c_char,
    key: &str,
    value: &str,
) -> Option<*const *const c_char> {
    let new_entry = CString::new(format!("{key}={value}")).ok()?;
    let has_key = |entry: &CStr| {
        entry
            .to_bytes()
            .strip_prefix(key.as_bytes())
            .is_some_and(|rest| rest.starts_with(b"="))
    };

    // `execve` accepts a null `envp` as an empty environment.
    let entries = (0..).map_while(|index| {
        let entry = (!envp.is_null()).then(|| unsafe { *envp.add(index) })?;
        (!entry.is_null()).then(|| unsafe { CStr::from_ptr(entry) })
    });

    if entries.clone().find(|entry| has_key(entry)) == Some(new_entry.as_c_str()) {
        return None;
    }

    let mut new_entry = Some(new_entry);
    let mut list = Vec::new();
    for entry in entries {
        if has_key(entry) {
            list.extend(new_entry.take().map(|new| new.into_raw().cast_const()));
        } else {
            list.push(entry.as_ptr());
        }
    }
    list.extend(new_entry.map(|new| new.into_raw().cast_const()));
    list.push(ptr::null());

    Some(list.leak().as_ptr())
}
