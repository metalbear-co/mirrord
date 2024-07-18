use std::{
    ffi::{c_char, CString},
    marker::PhantomData,
    ptr,
};

pub(crate) mod hooks;

/// Hold a vector of new CStrings to use instead of the original argv.
#[derive(Default, Debug)]
pub(crate) struct Argv(Vec<CString>);

/// This must be memory-same as just a `*const c_char`.
#[repr(C)]
pub(crate) struct StringPtr<'a> {
    ptr: *const c_char,
    _phantom: PhantomData<&'a ()>,
}

impl Argv {
    /// Get a null-pointer [`StringPtr`].
    pub(crate) fn null_string_ptr() -> StringPtr<'static> {
        StringPtr {
            ptr: ptr::null(),
            _phantom: Default::default(),
        }
    }

    /// Get a vector of pointers of which the data buffer is memory-same as a null-terminated array
    /// of pointers to null-terminated strings.
    pub(crate) fn null_vec(&self) -> Vec<StringPtr> {
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

    pub(crate) fn push(&mut self, item: CString) {
        self.0.push(item);
    }
}

impl FromIterator<CString> for Argv {
    fn from_iter<T: IntoIterator<Item = CString>>(iter: T) -> Self {
        Argv(Vec::from_iter(iter))
    }
}
