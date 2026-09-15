//! A Windows resource that releases itself on drop.
//!
//! The crash channel and the per-pid watch each hold several handles and one mapped view. Wrapping
//! them in [`OwnedHandle`] means neither side needs a hand-written close cascade on an error path
//! or at teardown — each value frees itself the right way.

use winapi::{
    ctypes::c_void,
    shared::{
        ntdef::HANDLE,
        windef::{HBITMAP, HBRUSH, HFONT, HGDIOBJ},
    },
    um::{handleapi::CloseHandle, memoryapi::UnmapViewOfFile, wingdi::DeleteObject},
};

/// A Windows resource owned for the lifetime of the value, freed on drop.
///
/// The variants exist because the resources we hold are freed by different calls: a kernel object
/// handle with `CloseHandle`, a mapped view with `UnmapViewOfFile`, a GDI object (font, brush,
/// bitmap) with `DeleteObject`.
pub(crate) enum OwnedHandle {
    /// A kernel object handle — a process, an event, or a file mapping. Closed with `CloseHandle`.
    Kernel(HANDLE),
    /// A mapped view of a file mapping. Released with `UnmapViewOfFile`.
    View(*mut c_void),
    /// A GDI object — a font, brush, or bitmap. Freed with `DeleteObject`.
    Gdi(HGDIOBJ),
}

// The wrapped resource is owned for this value's lifetime and only touched behind the crash
// channel's event synchronization, so it is safe to move across threads.
unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    /// Wraps a kernel handle from a `Create*` / `Open*` call.
    ///
    /// # Arguments
    ///
    /// * `handle` - the returned handle.
    ///
    /// # Returns
    ///
    /// The owned handle, or `None` when `handle` is null (the call failed).
    pub(crate) fn kernel(handle: HANDLE) -> Option<Self> {
        (!handle.is_null()).then_some(Self::Kernel(handle))
    }

    /// Wraps a mapped view from `MapViewOfFile`.
    ///
    /// # Arguments
    ///
    /// * `view` - the returned base address.
    ///
    /// # Returns
    ///
    /// The owned view, or `None` when `view` is null (the call failed).
    pub(crate) fn view(view: *mut c_void) -> Option<Self> {
        (!view.is_null()).then_some(Self::View(view))
    }

    /// The raw pointer, to hand back to the Win32 APIs.
    ///
    /// # Returns
    ///
    /// The wrapped resource as a raw pointer.
    pub(crate) fn as_ptr(&self) -> *mut c_void {
        match self {
            Self::Kernel(handle) | Self::Gdi(handle) => *handle,
            Self::View(view) => *view,
        }
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            match self {
                Self::Kernel(handle) => {
                    CloseHandle(*handle);
                }
                Self::View(view) => {
                    UnmapViewOfFile(*view);
                }
                Self::Gdi(object) => {
                    DeleteObject(*object);
                }
            }
        }
    }
}

// Fonts, brushes, and bitmaps are distinct opaque handle types but all are freed by `DeleteObject`.
// These conversions erase the concrete type into `Gdi`, so the dialog owns each one as an
// `OwnedHandle` via `.into()`. A null handle is kept and freed as a harmless `DeleteObject` no-op,
// so unlike the kernel and view constructors these never gate on null.
impl From<HFONT> for OwnedHandle {
    fn from(object: HFONT) -> Self {
        Self::Gdi(object as HGDIOBJ)
    }
}

impl From<HBRUSH> for OwnedHandle {
    fn from(object: HBRUSH) -> Self {
        Self::Gdi(object as HGDIOBJ)
    }
}

impl From<HBITMAP> for OwnedHandle {
    fn from(object: HBITMAP) -> Self {
        Self::Gdi(object as HGDIOBJ)
    }
}
