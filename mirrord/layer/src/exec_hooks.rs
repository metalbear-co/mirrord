#[cfg(target_os = "macos")]
use std::marker::PhantomData;
use std::{
    ffi::{c_char, CString},
    ptr,
};

pub(crate) mod hooks;

/// Hold a vector of new CStrings to use instead of the original argv.
#[derive(Default, Debug)]
pub(crate) struct Argv(Vec<CString>);

/// This must be memory-same as just a `*const c_char`.
#[cfg(target_os = "macos")]
#[repr(C)]
pub(crate) struct StringPtr<'a> {
    ptr: *const c_char,
    _phantom: PhantomData<&'a ()>,
}

impl Argv {
    /// Get a null-pointer [`StringPtr`].
    #[cfg(target_os = "macos")]
    pub(crate) fn null_string_ptr() -> StringPtr<'static> {
        StringPtr {
            ptr: ptr::null(),
            _phantom: Default::default(),
        }
    }

    /// Get a vector of pointers of which the data buffer is memory-same as a null-terminated array
    /// of pointers to null-terminated strings.
    #[cfg(target_os = "macos")]
    pub(crate) fn null_vec(&mut self) -> Vec<StringPtr> {
        let mut vec: Vec<StringPtr> = self
            .0
            .iter()
            .map(|c_string| StringPtr {
                ptr: c_string.as_ptr(),
                _phantom: Default::default(),
            })
            .collect();
        vec.push(Self::null_string_ptr());
        vec
    }

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
