//! Control construction for the crash dialog.
//!
//! [`build`] creates the logo, the title and subtitle, the report box, and the buttons, then
//! returns the owned [`DialogState`]. The parent module draws and drives them.

use str_win::string_to_u16_buffer;
use winapi::{
    shared::{
        minwindef::{HINSTANCE, LPARAM, WPARAM},
        windef::{HBITMAP, HFONT, HMENU, HWND},
    },
    um::{
        libloaderapi::GetModuleHandleW,
        wingdi::CreateSolidBrush,
        winuser::{
            BS_DEFPUSHBUTTON, BS_OWNERDRAW, CreateWindowExW, ES_AUTOVSCROLL, ES_MULTILINE,
            ES_READONLY, IMAGE_BITMAP, LR_DEFAULTCOLOR, LoadImageW, MAKEINTRESOURCEW, SS_BITMAP,
            SS_LEFT, STM_SETIMAGE, SendMessageW, WM_SETFONT, WS_BORDER, WS_CHILD, WS_TABSTOP,
            WS_VISIBLE, WS_VSCROLL,
        },
    },
};

use super::{
    state::{DialogInput, DialogState},
    theme::{
        ACCENT_HEIGHT, BAND_COLOR, BUTTON_FONT_HEIGHT, BUTTON_FONT_NAME, BUTTON_HEIGHT,
        DIALOG_HEIGHT, DIALOG_WIDTH, FOOTER_HEIGHT, HEADER_HEIGHT, ID_CLOSE, ID_COPY, ID_OPEN,
        ID_REPORT, LOGO_RESOURCE_ID, MARGIN, MONO_FONT_HEIGHT, MONO_FONT_NAME, PRESSED_SCALE,
        SUBTITLE_FONT_HEIGHT, SUBTITLE_FONT_NAME, TEXT_LEFT, TITLE_FONT_HEIGHT, TITLE_FONT_NAME,
        WHITE, create_font, scale, system_accent,
    },
};

/// Rich-edit control messages and flags (from `richedit.h`). Defined locally because winapi 0.3.9
/// ships no richedit module, so there is nothing to import these from.
const EM_SETEVENTMASK: u32 = 0x0445;
const EM_SETBKGNDCOLOR: u32 = 0x0443;
/// `EN_LINK` notifications, requested via [`EM_SETEVENTMASK`].
const ENM_LINK: u32 = 0x0400_0000;

/// Builds the controls and the owned GDI resources for the window.
///
/// # Arguments
///
/// * `window` - the parent dialog window.
/// * `input` - the caller's title, subtitle, body, and action targets.
///
/// # Returns
///
/// The owned [`DialogState`].
pub(super) unsafe fn build(window: HWND, input: &DialogInput) -> DialogState {
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };

    let title_font = unsafe { create_font(TITLE_FONT_NAME, TITLE_FONT_HEIGHT, true) };
    let subtitle_font = unsafe { create_font(SUBTITLE_FONT_NAME, SUBTITLE_FONT_HEIGHT, false) };
    let mono_font = unsafe { create_font(MONO_FONT_NAME, MONO_FONT_HEIGHT, false) };
    let button_font = unsafe { create_font(BUTTON_FONT_NAME, BUTTON_FONT_HEIGHT, false) };
    let band_brush = unsafe { CreateSolidBrush(BAND_COLOR) };
    let white_brush = unsafe { CreateSolidBrush(WHITE) };
    let accent = system_accent();

    let static_class = string_to_u16_buffer("STATIC");
    let empty = string_to_u16_buffer("");

    // Logo on the band.
    let logo = unsafe {
        CreateWindowExW(
            0,
            static_class.as_ptr(),
            empty.as_ptr(),
            WS_CHILD | WS_VISIBLE | SS_BITMAP,
            MARGIN,
            14,
            96,
            96,
            window,
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        )
    };
    let logo_bitmap = unsafe {
        LoadImageW(
            instance,
            MAKEINTRESOURCEW(LOGO_RESOURCE_ID),
            IMAGE_BITMAP,
            0,
            0,
            LR_DEFAULTCOLOR,
        ) as HBITMAP
    };
    if !logo_bitmap.is_null() {
        unsafe {
            SendMessageW(
                logo,
                STM_SETIMAGE,
                IMAGE_BITMAP as WPARAM,
                logo_bitmap as LPARAM,
            )
        };
    }

    let text_width = DIALOG_WIDTH - TEXT_LEFT - MARGIN;

    // Bold title and lighter subtitle on the band, stacked beside the logo.
    let title_window = unsafe {
        CreateWindowExW(
            0,
            static_class.as_ptr(),
            input.title.as_ptr(),
            WS_CHILD | WS_VISIBLE | SS_LEFT,
            TEXT_LEFT,
            40,
            text_width,
            28,
            window,
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        )
    };
    unsafe { SendMessageW(title_window, WM_SETFONT, title_font as WPARAM, 1) };

    let subtitle_window = unsafe {
        CreateWindowExW(
            0,
            static_class.as_ptr(),
            input.subtitle.as_ptr(),
            WS_CHILD | WS_VISIBLE | SS_LEFT,
            TEXT_LEFT,
            72,
            text_width,
            22,
            window,
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        )
    };
    unsafe { SendMessageW(subtitle_window, WM_SETFONT, subtitle_font as WPARAM, 1) };

    // The scrollable report box is a rich-edit control. The report's markdown links are rendered as
    // blue, clickable `CFE_LINK` runs by `apply_links`; the window proc opens them on `EN_LINK`.
    // Loading `Msftedit.dll` registers the `RICHEDIT50W` window class.
    let report_top = HEADER_HEIGHT + ACCENT_HEIGHT + 12;
    let report_bottom = DIALOG_HEIGHT - FOOTER_HEIGHT - 12;
    crate::process::load_system_library("Msftedit.dll");
    let report_class = string_to_u16_buffer("RICHEDIT50W");
    let report_window = unsafe {
        CreateWindowExW(
            0,
            report_class.as_ptr(),
            empty.as_ptr(),
            WS_CHILD
                | WS_VISIBLE
                | WS_BORDER
                | WS_VSCROLL
                | WS_TABSTOP
                | ES_MULTILINE
                | ES_READONLY
                | ES_AUTOVSCROLL,
            MARGIN,
            report_top,
            DIALOG_WIDTH - 2 * MARGIN,
            report_bottom - report_top,
            window,
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        )
    };
    unsafe {
        SendMessageW(report_window, WM_SETFONT, mono_font as WPARAM, 1);
        // Request `EN_LINK` notifications and force the white background.
        SendMessageW(report_window, EM_SETEVENTMASK, 0, ENM_LINK as LPARAM);
        SendMessageW(report_window, EM_SETBKGNDCOLOR, 0, WHITE as LPARAM);
    }
    // Fill the report box from its segments and capture the link ranges for the `EN_LINK` handler.
    let links = unsafe { super::apply_content(report_window, &input.segments) };

    // Flat owner-drawn buttons in the footer band. Close on the left, the action group on the
    // right.
    let button_y = DIALOG_HEIGHT - FOOTER_HEIGHT + (FOOTER_HEIGHT - BUTTON_HEIGHT) / 2;
    let report_x = DIALOG_WIDTH - MARGIN - 132;
    let copy_x = report_x - 8 - 84;
    let open_x = copy_x - 8 - 112;
    create_button(
        window,
        instance,
        button_font,
        "Close",
        ID_CLOSE,
        MARGIN,
        button_y,
        92,
    );
    create_button(
        window,
        instance,
        button_font,
        "Open folder",
        ID_OPEN,
        open_x,
        button_y,
        112,
    );
    create_button(
        window,
        instance,
        button_font,
        "Copy",
        ID_COPY,
        copy_x,
        button_y,
        84,
    );
    create_button(
        window,
        instance,
        button_font,
        "Report Crash",
        ID_REPORT,
        report_x,
        button_y,
        132,
    );

    DialogState {
        body: input.body.clone(),
        directory: input.directory.clone(),
        issue_url: input.issue_url.clone(),
        links,
        title_window,
        subtitle_window,
        report_window,
        band_brush: band_brush.into(),
        white_brush: white_brush.into(),
        _retained_gdi: [
            title_font.into(),
            subtitle_font.into(),
            mono_font.into(),
            button_font.into(),
            logo_bitmap.into(),
        ],
        accent,
        accent_pressed: scale(accent, PRESSED_SCALE),
    }
}

/// Creates one flat owner-drawn push button.
///
/// Exactly one button is the default (the primary action), so Enter has a single, correct target.
///
/// # Arguments
///
/// * `parent` - the dialog window.
/// * `instance` - the module instance.
/// * `font` - the button label font.
/// * `text` - the button label.
/// * `id` - the control id.
/// * `x` - the left position.
/// * `y` - the top position.
/// * `width` - the button width.
#[allow(clippy::too_many_arguments)]
fn create_button(
    parent: HWND,
    instance: HINSTANCE,
    font: HFONT,
    text: &str,
    id: usize,
    x: i32,
    y: i32,
    width: i32,
) {
    let class = string_to_u16_buffer("BUTTON");
    let label = string_to_u16_buffer(text);
    let default = if id == ID_REPORT { BS_DEFPUSHBUTTON } else { 0 };
    let style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW | default;
    let button = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            label.as_ptr(),
            style,
            x,
            y,
            width,
            BUTTON_HEIGHT,
            parent,
            id as HMENU,
            instance,
            std::ptr::null_mut(),
        )
    };
    unsafe { SendMessageW(button, WM_SETFONT, font as WPARAM, 1) };
}
