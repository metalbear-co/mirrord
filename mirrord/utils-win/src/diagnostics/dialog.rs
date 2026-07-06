//! The native crash dialog — SA-MP-inspired, tuned for Windows 11.
//!
//! A custom Win32 window with three zones: a dark branded header band (logo, a bold title, a
//! lighter subtitle) capped by an accent stripe, a body with a read-only monospace report box, and
//! a footer band with flat owner-drawn buttons (Report Crash / Copy / Open folder / Close).
//!
//! It is shown by the monitor, never by a crashing process. The logo is an embedded bitmap resource
//! (`logo.rc`, compiled by `build.rs`); it is a placeholder until the real metalbear asset lands.
//!
//! The pieces split across submodules: [`theme`] holds the look (geometry, palette, fonts),
//! [`state`] the per-window data, and [`build`] the control construction. This file owns the
//! window: its message loop, painting, and the button actions.

mod build;
mod state;
mod theme;

use std::path::Path;

use str_win::string_to_u16_buffer;
use theme::*;
use winapi::{
    ctypes::c_void,
    shared::{
        minwindef::{LPARAM, LRESULT, UINT, WPARAM},
        windef::{COLORREF, HBRUSH, HDC, HFONT, HPEN, HWND, RECT},
    },
    um::{
        dwmapi::DwmSetWindowAttribute,
        libloaderapi::GetModuleHandleW,
        shellapi::ShellExecuteW,
        wingdi::{
            CreatePen, CreateSolidBrush, DeleteObject, GetStockObject, NULL_BRUSH, NULL_PEN,
            PS_SOLID, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
        },
        winuser::{
            AdjustWindowRect, CREATESTRUCTW, CW_USEDEFAULT, CreateWindowExW, DRAWITEMSTRUCT,
            DT_CENTER, DT_SINGLELINE, DT_VCENTER, DefWindowProcW, DestroyWindow, DispatchMessageW,
            DrawTextW, FillRect, GWLP_USERDATA, GetClientRect, GetMessageW, GetParent,
            GetSystemMetrics, GetWindowLongPtrW, GetWindowTextW, IDC_ARROW, IsDialogMessageW,
            LoadCursorW, MSG, ODS_SELECTED, PostQuitMessage, RegisterClassExW, SM_CXSCREEN,
            SM_CYSCREEN, SW_SHOWNORMAL, SendMessageW, SetForegroundWindow, SetWindowLongPtrW,
            ShowWindow, TranslateMessage, WM_COMMAND, WM_CREATE, WM_CTLCOLORSTATIC, WM_DESTROY,
            WM_DRAWITEM, WM_ERASEBKGND, WM_GETFONT, WM_LBUTTONUP, WM_NOTIFY, WNDCLASSEXW,
            WS_CAPTION, WS_SYSMENU,
        },
    },
};

use self::state::{DialogInput, DialogState, LinkSpan, Segment};
use crate::clipboard;

/// Shows the crash dialog and blocks until the user closes it.
///
/// # Arguments
///
/// * `title` - the bold header line, e.g. `mirrord caught a crash`.
/// * `subtitle` - the lighter line below it, e.g. `crasher (pid 32908)`.
/// * `body` - the full report text shown in the scrollable box.
/// * `directory` - the crash-artifact folder, opened by the Open-folder button.
/// * `issue_url` - the GitHub new-issue URL, opened by the Report button.
pub fn show(title: &str, subtitle: &str, body: &str, directory: &Path, issue_url: &str) {
    let input = DialogInput {
        title: string_to_u16_buffer(title),
        subtitle: string_to_u16_buffer(subtitle),
        // The Copy button takes the markdown source so the raw URLs travel with the copied text;
        // the rich-edit control wants CRLF line breaks.
        body: string_to_u16_buffer(body.replace('\n', "\r\n")),
        segments: parse_segments(body),
        directory: string_to_u16_buffer(directory.to_string_lossy()),
        issue_url: string_to_u16_buffer(issue_url),
    };

    unsafe { run_dialog(&input) };
}

/// Splits the report's markdown into plain and link runs.
///
/// The report uses `[label](url)`. Plain text accumulates into one segment until a link, where the
/// label becomes a link segment carrying its URL; the URL itself is never shown. Line breaks are
/// emitted as a lone `\r`, which the rich-edit control accepts. A `[` that is not part of a
/// well-formed `[label](url)` (e.g. a process-tree `[exited ...]` tag) is treated as plain text.
fn parse_segments(markdown: &str) -> Vec<Segment> {
    let chars: Vec<char> = markdown.chars().collect();
    let mut segments: Vec<Segment> = Vec::new();
    let mut plain = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '['
            && let Some(close) = find_on_line(&chars, index + 1, ']')
            && chars.get(close + 1) == Some(&'(')
            && let Some(end) = find_on_line(&chars, close + 2, ')')
        {
            if !plain.is_empty() {
                segments.push(Segment {
                    text: string_to_u16_buffer(&plain),
                    url: None,
                });
                plain.clear();
            }
            let label: String = chars[index + 1..close].iter().collect();
            let url: String = chars[close + 2..end].iter().collect();
            segments.push(Segment {
                text: string_to_u16_buffer(label),
                url: Some(string_to_u16_buffer(url)),
            });
            index = end + 1;
            continue;
        }

        plain.push(if chars[index] == '\n' {
            '\r'
        } else {
            chars[index]
        });
        index += 1;
    }
    if !plain.is_empty() {
        segments.push(Segment {
            text: string_to_u16_buffer(plain),
            url: None,
        });
    }
    segments
}

/// Finds `target` in `chars` starting at `from`, stopping at a line break so a link never spans
/// lines. Returns `None` if a line break or the end is reached first.
fn find_on_line(chars: &[char], from: usize, target: char) -> Option<usize> {
    chars
        .get(from..)?
        .iter()
        .position(|&c| c == target || c == '\n')
        .map(|offset| from + offset)
        .filter(|&found| chars[found] == target)
}

/// Registers the window class, creates the dialog, and pumps its message loop.
///
/// # Arguments
///
/// * `input` - the caller data, borrowed for the duration of the call.
unsafe fn run_dialog(input: &DialogInput) {
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let class_name = string_to_u16_buffer("MirrordCrashDialog");

    let window_class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(window_proc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: std::ptr::null_mut(),
        hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
        hbrBackground: (6) as HBRUSH, // COLOR_WINDOW + 1
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: std::ptr::null_mut(),
    };
    // Re-registration after the first call returns 0; that is fine.
    unsafe { RegisterClassExW(&window_class) };

    let title = string_to_u16_buffer("mirrord crash");
    let style = WS_CAPTION | WS_SYSMENU;

    // Size the window so the client area is exactly the dialog dimensions.
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: DIALOG_WIDTH,
        bottom: DIALOG_HEIGHT,
    };
    unsafe { AdjustWindowRect(&mut rect, style, 0) };
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let (x, y) = centered_position(width, height);

    let window = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            x,
            y,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::from_ref(input) as *mut _,
        )
    };
    if window.is_null() {
        return;
    }

    unsafe {
        apply_window11_chrome(window);
        ShowWindow(window, SW_SHOWNORMAL);
        SetForegroundWindow(window);
    }

    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) } > 0 {
        // `IsDialogMessageW` gives real keyboard navigation across the `WS_TABSTOP` children: Tab
        // and Shift-Tab move focus, Enter fires the default button. It returns nonzero when
        // it consumed the message.
        if unsafe { IsDialogMessageW(window, &mut message) } == 0 {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

/// Applies the Windows 11 chrome: a dark title bar and rounded corners.
///
/// Both attributes degrade gracefully on older Windows. The dark-mode attribute id moved from 19 to
/// 20, so this tries the modern id and falls back to the legacy one when it is rejected. Rounded
/// corners are Windows 11 only and are simply ignored on Windows 10.
///
/// # Arguments
///
/// * `window` - the dialog window.
unsafe fn apply_window11_chrome(window: HWND) {
    unsafe {
        let dark: i32 = 1;
        let dark_ptr = std::ptr::from_ref(&dark) as *const c_void;
        let size = std::mem::size_of::<i32>() as u32;

        // A failed `DwmSetWindowAttribute` returns a negative `HRESULT`. If the modern dark-mode id
        // is rejected (older Windows 10), retry with the legacy id.
        let result = DwmSetWindowAttribute(window, DWMWA_USE_IMMERSIVE_DARK_MODE, dark_ptr, size);
        if result < 0 {
            DwmSetWindowAttribute(window, DWMWA_USE_IMMERSIVE_DARK_MODE_OLD, dark_ptr, size);
        }

        let corner: u32 = DWMWCP_ROUND;
        DwmSetWindowAttribute(
            window,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            std::ptr::from_ref(&corner) as *const c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

/// Returns a top-left position that centers a window of the given size.
///
/// # Arguments
///
/// * `width` - the window width.
/// * `height` - the window height.
///
/// # Returns
///
/// The centered top-left, or [`CW_USEDEFAULT`] when the screen size is unavailable.
fn centered_position(width: i32, height: i32) -> (i32, i32) {
    let screen_width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let screen_height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    if screen_width <= 0 || screen_height <= 0 {
        return (CW_USEDEFAULT, CW_USEDEFAULT);
    }
    ((screen_width - width) / 2, (screen_height - height) / 2)
}

/// The dialog window procedure.
unsafe extern "system" fn window_proc(
    window: HWND,
    message: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CREATE => {
            let create = lparam as *const CREATESTRUCTW;
            let input = unsafe { (*create).lpCreateParams } as *const DialogInput;
            let state = unsafe { build::build(window, &*input) };
            let state = Box::into_raw(Box::new(state));
            unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, state as isize) };
            0
        }
        WM_ERASEBKGND => {
            unsafe { paint_background(window, wparam as HDC) };
            1
        }
        WM_CTLCOLORSTATIC => {
            if let Some(state) = unsafe { state_ref(window) } {
                return unsafe { color_static(state, wparam as HDC, lparam as HWND) };
            }
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_DRAWITEM => {
            let item = lparam as *const DRAWITEMSTRUCT;
            unsafe { draw_button(&*item) };
            1
        }
        WM_COMMAND => {
            let control_id = wparam & 0xFFFF;
            if let Some(state) = unsafe { state_ref(window) } {
                unsafe { handle_command(window, state, control_id) };
            }
            0
        }
        WM_NOTIFY => {
            // The report box reports a click on a `CFE_LINK` range via `EN_LINK`; map the clicked
            // character position back to the markdown link's URL and open it on mouse-up.
            let header = lparam as *const Nmhdr;
            if !header.is_null() && unsafe { (*header).code } == EN_LINK {
                let link = lparam as *const EnLink;
                let link_msg = unsafe { (*link).msg };
                // TEMP DEBUG: log only button events to avoid mousemove spam.
                if link_msg == WM_LBUTTONUP
                    && let Some(state) = unsafe { state_ref(window) }
                {
                    // Match the span containing the clicked position. The end is inclusive because
                    // a click at the link's trailing boundary reports `cp ==
                    // end`. The spans are separated by plain text, so the
                    // inclusive end never collides with another link.
                    let cp = unsafe { (*link).chrg.cp_min };
                    if let Some(span) = state
                        .links
                        .iter()
                        .find(|span| cp >= span.start && cp <= span.end)
                    {
                        shell_open(&span.url);
                    }
                }
                return 0;
            }
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_DESTROY => {
            let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut DialogState;
            if !pointer.is_null() {
                drop(unsafe { Box::from_raw(pointer) });
                unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, 0) };
            }
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// Borrows the per-window state, if set.
///
/// # Arguments
///
/// * `window` - the dialog window holding the state pointer.
///
/// # Returns
///
/// A mutable borrow of the state, or `None` before `WM_CREATE` stored it.
unsafe fn state_ref<'a>(window: HWND) -> Option<&'a mut DialogState> {
    let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *mut DialogState;
    unsafe { pointer.as_mut() }
}

/// Paints the three zones: white body, dark header band with an accent stripe, and a footer band.
///
/// # Arguments
///
/// * `window` - the dialog window.
/// * `hdc` - the device context to paint into.
unsafe fn paint_background(window: HWND, hdc: HDC) {
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetClientRect(window, &mut client) };

    let accent = unsafe { state_ref(window) }
        .map(|state| state.accent)
        .unwrap_or(ACCENT_FALLBACK);
    let footer_top = client.bottom - FOOTER_HEIGHT;
    unsafe {
        fill(hdc, 0, 0, client.right, client.bottom, WHITE);
        fill(hdc, 0, 0, client.right, HEADER_HEIGHT, BAND_COLOR);
        fill(
            hdc,
            0,
            HEADER_HEIGHT,
            client.right,
            HEADER_HEIGHT + ACCENT_HEIGHT,
            accent,
        );
        fill(hdc, 0, footer_top, client.right, client.bottom, FOOTER_BG);
        fill(hdc, 0, footer_top, client.right, footer_top + 1, HAIRLINE);
    }
}

/// Fills a rectangle with a solid color.
unsafe fn fill(hdc: HDC, left: i32, top: i32, right: i32, bottom: i32, color: COLORREF) {
    let rect = RECT {
        left,
        top,
        right,
        bottom,
    };
    let brush = unsafe { CreateSolidBrush(color) };
    unsafe {
        FillRect(hdc, &rect, brush);
        DeleteObject(brush as _);
    }
}

/// Draws a flat, rounded button: accent-filled for the primary action, outlined for the rest.
///
/// # Arguments
///
/// * `item` - the owner-draw item from `WM_DRAWITEM`.
unsafe fn draw_button(item: &DRAWITEMSTRUCT) {
    let hdc = item.hDC;
    let rect = item.rcItem;
    let pressed = item.itemState & ODS_SELECTED != 0;
    let primary = item.CtlID as usize == ID_REPORT;

    let parent = unsafe { GetParent(item.hwndItem) };
    let (accent, accent_pressed) = unsafe { state_ref(parent) }
        .map(|state| (state.accent, state.accent_pressed))
        .unwrap_or((ACCENT_FALLBACK, scale(ACCENT_FALLBACK, PRESSED_SCALE)));

    let (fill_color, text_color, border) = if primary {
        let fill = if pressed { accent_pressed } else { accent };
        (fill, WHITE, None)
    } else {
        let fill = if pressed { BTN_PRESSED } else { WHITE };
        (fill, TEXT_DARK, Some(BTN_BORDER))
    };

    unsafe {
        let brush = CreateSolidBrush(fill_color);
        let pen = match border {
            Some(color) => CreatePen(PS_SOLID as i32, 1, color),
            None => GetStockObject(NULL_PEN as i32) as HPEN,
        };
        let old_brush = SelectObject(hdc, brush as _);
        let old_pen = SelectObject(hdc, pen as _);
        RoundRect(
            hdc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            BUTTON_RADIUS,
            BUTTON_RADIUS,
        );
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_pen);
        DeleteObject(brush as _);
        if border.is_some() {
            DeleteObject(pen as _);
        }

        // Center the label using the button's own font.
        let font = SendMessageW(item.hwndItem, WM_GETFONT, 0, 0) as HFONT;
        if !font.is_null() {
            SelectObject(hdc, font as _);
        }
        let mut label = [0u16; 64];
        let len = GetWindowTextW(item.hwndItem, label.as_mut_ptr(), label.len() as i32);
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(hdc, text_color);
        let mut text_rect = rect;
        DrawTextW(
            hdc,
            label.as_ptr(),
            len,
            &mut text_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }
}

/// Colors the title, the subtitle, and the report box.
///
/// # Arguments
///
/// * `state` - the per-window state.
/// * `hdc` - the control's device context.
/// * `control` - the static control being colored.
///
/// # Returns
///
/// The background brush for the control.
unsafe fn color_static(state: &DialogState, hdc: HDC, control: HWND) -> LRESULT {
    if control == state.title_window || control == state.subtitle_window {
        let color = if control == state.title_window {
            TITLE_COLOR
        } else {
            SUBTITLE_COLOR
        };
        unsafe {
            SetTextColor(hdc, color);
            SetBkMode(hdc, TRANSPARENT as i32);
            return GetStockObject(NULL_BRUSH as i32) as LRESULT;
        }
    }
    if control == state.report_window {
        // A read-only edit needs an OPAQUE background. With a transparent mode, a partial repaint
        // draws text over the old text and smears the top lines.
        unsafe {
            SetTextColor(hdc, TEXT_DARK);
            SetBkColor(hdc, WHITE);
            return state.white_brush.as_ptr() as LRESULT;
        }
    }
    // Other statics (the logo) keep the band behind them.
    unsafe {
        SetBkMode(hdc, TRANSPARENT as i32);
        state.band_brush.as_ptr() as LRESULT
    }
}

/// Runs the action for a clicked button.
///
/// # Arguments
///
/// * `window` - the dialog window.
/// * `state` - the per-window state holding the action targets.
/// * `control_id` - the id of the clicked button.
unsafe fn handle_command(window: HWND, state: &DialogState, control_id: usize) {
    match control_id {
        ID_REPORT => shell_open(&state.issue_url),
        ID_OPEN => shell_open(&state.directory),
        ID_COPY => clipboard::set_text(window, &state.body),
        ID_CLOSE => unsafe {
            DestroyWindow(window);
        },
        _ => {}
    }
}

/// Opens a target (URL or folder) with the shell.
///
/// # Arguments
///
/// * `target` - a null-terminated wide URL or path.
fn shell_open(target: &[u16]) {
    let verb = string_to_u16_buffer("open");
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

// The rich-edit messages, flags, notification code, and payload structs below are declared locally
// because winapi 0.3.9 ships no richedit module to import them from.

/// `EN_LINK` notification code, sent by the rich-edit control for a `CFE_LINK` range.
const EN_LINK: u32 = 0x070B;
/// `EM_EXGETSEL` — reads the selection as a character range.
const EM_EXGETSEL: u32 = 0x0434;
/// `EM_EXSETSEL` — sets the selection by character range.
const EM_EXSETSEL: u32 = 0x0437;
/// `EM_REPLACESEL` — replaces the selection with text, inserting at an empty selection.
const EM_REPLACESEL: u32 = 0x00C2;
/// `EM_SETREADONLY` — toggles the read-only protection that otherwise blocks programmatic edits.
const EM_SETREADONLY: u32 = 0x00CF;
/// `EM_SETCHARFORMAT` — applies character formatting.
const EM_SETCHARFORMAT: u32 = 0x0444;
/// Apply the format to the current selection only.
const SCF_SELECTION: usize = 0x0001;
/// Link mask and effect bits for [`CharFormat2W`].
const CFM_LINK: u32 = 0x0000_0020;
const CFE_LINK: u32 = 0x0000_0020;

/// The `NMHDR` prefix shared by every `WM_NOTIFY` payload; mapped from the notification pointer.
#[repr(C)]
#[allow(dead_code)]
struct Nmhdr {
    hwnd_from: HWND,
    id_from: usize,
    code: u32,
}

/// A `[cp_min, cp_max)` character range in the rich-edit control.
#[repr(C)]
#[derive(Clone, Copy)]
struct CharRange {
    cp_min: i32,
    cp_max: i32,
}

/// The `ENLINK` payload of an `EN_LINK` notification.
#[repr(C)]
#[allow(dead_code)]
struct EnLink {
    nmhdr: Nmhdr,
    msg: u32,
    w_param: WPARAM,
    l_param: LPARAM,
    chrg: CharRange,
}

/// `CHARFORMAT2W` (from `richedit.h`), used here only to set the link effect. The field layout and
/// `cbSize` must match the C struct exactly, so the full struct is declared even though only the
/// mask and effect bits are set.
#[repr(C)]
#[allow(dead_code)]
struct CharFormat2W {
    cb_size: u32,
    dw_mask: u32,
    dw_effects: u32,
    y_height: i32,
    y_offset: i32,
    cr_text_color: u32,
    b_char_set: u8,
    b_pitch_and_family: u8,
    sz_face_name: [u16; 32],
    w_weight: u16,
    s_spacing: i16,
    cr_back_color: u32,
    lcid: u32,
    dw_cookie: u32,
    s_style: i16,
    w_kerning: u16,
    b_underline_type: u8,
    b_animation: u8,
    b_rev_author: u8,
    b_underline_color: u8,
}

/// Builds the report box from its segments and returns the clickable link ranges.
///
/// Each segment is appended with `EM_REPLACESEL`, and the control is asked where the caret landed
/// before and after a link run, so the recorded range is the control's own character positions, not
/// an offset computed here. URL auto-detection stays off; the link runs are then given the link
/// effect explicitly, which the rich-edit control renders blue and underlined and reports via
/// `EN_LINK`. The selection is cleared afterwards so no text is left highlighted.
///
/// # Arguments
///
/// * `report` - the rich-edit report control, expected to be empty.
/// * `segments` - the plain and link runs to insert, in order.
///
/// # Returns
///
/// The character range and URL of every link run.
unsafe fn apply_content(report: HWND, segments: &[Segment]) -> Vec<LinkSpan> {
    // A read-only rich-edit control silently ignores `EM_REPLACESEL` and `EM_SETCHARFORMAT`, so
    // drop read-only while building and styling the text, then restore it before returning.
    unsafe { SendMessageW(report, EM_SETREADONLY, 0, 0) };

    let mut links = Vec::new();
    let start = CharRange {
        cp_min: 0,
        cp_max: 0,
    };
    unsafe { SendMessageW(report, EM_EXSETSEL, 0, std::ptr::from_ref(&start) as LPARAM) };

    for segment in segments {
        let begin = unsafe { caret_position(report) };
        unsafe { SendMessageW(report, EM_REPLACESEL, 0, segment.text.as_ptr() as LPARAM) };
        if let Some(url) = &segment.url {
            let end = unsafe { caret_position(report) };
            links.push(LinkSpan {
                start: begin,
                end,
                url: url.clone(),
            });
        }
    }

    for span in &links {
        let range = CharRange {
            cp_min: span.start,
            cp_max: span.end,
        };
        let mut format: CharFormat2W = unsafe { std::mem::zeroed() };
        format.cb_size = std::mem::size_of::<CharFormat2W>() as u32;
        format.dw_mask = CFM_LINK;
        format.dw_effects = CFE_LINK;
        unsafe {
            SendMessageW(report, EM_EXSETSEL, 0, std::ptr::from_ref(&range) as LPARAM);
            SendMessageW(
                report,
                EM_SETCHARFORMAT,
                SCF_SELECTION,
                std::ptr::from_mut(&mut format) as LPARAM,
            );
        }
    }

    unsafe { SendMessageW(report, EM_EXSETSEL, 0, std::ptr::from_ref(&start) as LPARAM) };
    unsafe { SendMessageW(report, EM_SETREADONLY, 1, 0) };
    links
}

/// Reads the caret's character position (the start of the current selection) in the report box.
unsafe fn caret_position(report: HWND) -> i32 {
    let mut range = CharRange {
        cp_min: 0,
        cp_max: 0,
    };
    unsafe {
        SendMessageW(
            report,
            EM_EXGETSEL,
            0,
            std::ptr::from_mut(&mut range) as LPARAM,
        )
    };
    range.cp_min
}
