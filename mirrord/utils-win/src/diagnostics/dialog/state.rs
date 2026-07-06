//! The dialog's input and per-window state.
//!
//! [`DialogInput`] is the borrowed caller data passed through window creation. [`DialogState`] owns
//! the controls and GDI objects for the window's lifetime; each GDI object is an [`OwnedHandle`],
//! so they free themselves on drop with no hand-written cascade.

use winapi::shared::windef::{COLORREF, HWND};

use crate::diagnostics::handle::OwnedHandle;

/// A clickable link in the rendered report.
///
/// The dialog builds the report box one [`Segment`] at a time and asks the control where each link
/// ended up. This records that range, in the control's own character positions, and the URL the
/// shell opens when the range is clicked.
#[derive(Clone)]
pub(super) struct LinkSpan {
    pub(super) start: i32,
    pub(super) end: i32,
    pub(super) url: Vec<u16>,
}

/// One run of report text, either plain or a link.
///
/// The report uses markdown `[label](url)`; parsing splits it into segments so the dialog can
/// insert each run with `EM_REPLACESEL` and mark only the link runs. A link segment's `text` is its
/// label (the URL is hidden); `url` is the null-terminated target the shell opens.
pub(super) struct Segment {
    /// The null-terminated wide text to insert.
    pub(super) text: Vec<u16>,
    /// The null-terminated target URL when this run is a link.
    pub(super) url: Option<Vec<u16>>,
}

/// The caller data, borrowed only during window creation.
pub(super) struct DialogInput {
    pub(super) title: Vec<u16>,
    pub(super) subtitle: Vec<u16>,
    /// The markdown report text, used by the Copy button so the raw URLs travel with it.
    pub(super) body: Vec<u16>,
    /// The report split into plain and link runs, inserted in order to build the report box.
    pub(super) segments: Vec<Segment>,
    pub(super) directory: Vec<u16>,
    pub(super) issue_url: Vec<u16>,
}

/// Per-window runtime state, owned for the window's lifetime.
///
/// The text fields back the Copy and Open-folder actions. Every GDI object is an [`OwnedHandle`],
/// so they free themselves on drop with no hand-written cascade.
pub(super) struct DialogState {
    pub(super) body: Vec<u16>,
    pub(super) directory: Vec<u16>,
    pub(super) issue_url: Vec<u16>,
    /// The clickable link ranges in the report box, looked up on an `EN_LINK` click.
    pub(super) links: Vec<LinkSpan>,
    pub(super) title_window: HWND,
    pub(super) subtitle_window: HWND,
    pub(super) report_window: HWND,
    /// The control-background brushes, read while painting the `WM_CTLCOLOR*` messages.
    pub(super) band_brush: OwnedHandle,
    pub(super) white_brush: OwnedHandle,
    /// The fonts and the logo bitmap. The controls reference these without owning them, so they
    /// are retained here only to keep them alive for the window's lifetime and free them on
    /// drop.
    pub(super) _retained_gdi: [OwnedHandle; 5],
    pub(super) accent: COLORREF,
    pub(super) accent_pressed: COLORREF,
}
