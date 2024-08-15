use std::{
    ffi::{c_char, CString},
    ptr,
};

pub(crate) mod hooks;

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
