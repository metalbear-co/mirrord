use std::{
    ffi::{c_char, CString},
    ptr,
};

pub(crate) mod hooks;

/// Hold a vector of new CStrings to use instead of the original argv.
#[derive(Default, Debug)]
pub(crate) struct Argv(Vec<CString>);

impl Argv {
    pub(crate) fn leak(self) -> *const *const c_char {
        let mut list = self
            .0
            .into_iter()
            .map(|value| value.into_raw().cast_const())
            .collect::<Vec<_>>();

        list.push(ptr::null());

        list.into_raw_parts().0.cast_const()
    }

    pub(crate) fn push(&mut self, item: CString) {
        self.0.push(item);
    }
}

impl FromIterator<CString> for Argv {
    fn from_iter<T: IntoIterator<Item = CString>>(iter: T) -> Self {
        Argv(Vec::from_iter(iter))
    }
}
