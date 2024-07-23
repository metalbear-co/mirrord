use std::{
    ffi::{c_char, CString},
    ptr,
};

pub(crate) mod hooks;

/// Hold a vector of new CStrings to use instead of the original argv.
#[derive(Default, Debug)]
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
}

impl FromIterator<CString> for Argv {
    fn from_iter<T: IntoIterator<Item = CString>>(iter: T) -> Self {
        Argv(Vec::from_iter(iter))
    }
}
