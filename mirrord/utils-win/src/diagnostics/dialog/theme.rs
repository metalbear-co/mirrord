//! Look of the crash dialog: control ids, geometry, the palette, fonts, and the system accent.
//!
//! These are the knobs that decide how the dialog looks. The window logic in the parent module
//! draws against them.

use str_win::string_to_u16_buffer;
use winapi::{
    shared::windef::{COLORREF, HFONT},
    um::{
        dwmapi::DwmGetColorizationColor,
        wingdi::{
            CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH,
            FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD, OUT_TT_PRECIS,
        },
    },
};

/// Resource id of the embedded logo bitmap. See `logo.rc` / `build.rs`.
pub(super) const LOGO_RESOURCE_ID: u16 = 100;

/// Control id of the Report-Crash button. Also the one default button.
pub(super) const ID_REPORT: usize = 1001;
/// Control id of the Copy button.
pub(super) const ID_COPY: usize = 1002;
/// Control id of the Open-folder button.
pub(super) const ID_OPEN: usize = 1003;
/// Control id of the Close button.
pub(super) const ID_CLOSE: usize = 1004;

/// Client width of the dialog, in pixels.
pub(super) const DIALOG_WIDTH: i32 = 560;
/// Client height of the dialog, in pixels.
pub(super) const DIALOG_HEIGHT: i32 = 520;
/// Height of the dark header band.
pub(super) const HEADER_HEIGHT: i32 = 124;
/// Height of the footer band that holds the buttons.
pub(super) const FOOTER_HEIGHT: i32 = 62;
/// Height of the accent stripe under the header band.
pub(super) const ACCENT_HEIGHT: i32 = 3;
/// Outer padding used throughout the layout.
pub(super) const MARGIN: i32 = 16;
/// Height of each action button.
pub(super) const BUTTON_HEIGHT: i32 = 34;
/// Corner radius of the rounded buttons.
pub(super) const BUTTON_RADIUS: i32 = 6;
/// Left edge of the header text, clearing the logo.
pub(super) const TEXT_LEFT: i32 = MARGIN + 112;

/// `DwmSetWindowAttribute`: use the dark title bar (Windows 10 2004+ / Windows 11).
pub(super) const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
/// The dark-title-bar attribute id on Windows 10 1809–1909, before it moved to 20.
pub(super) const DWMWA_USE_IMMERSIVE_DARK_MODE_OLD: u32 = 19;
/// `DwmSetWindowAttribute`: window corner preference.
pub(super) const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
/// Round the window corners.
pub(super) const DWMWCP_ROUND: u32 = 2;

/// Header band color (dark navy-charcoal).
pub(super) const BAND_COLOR: COLORREF = rgb(0x1E, 0x22, 0x30);
/// Fallback accent / primary color (blue), used if the system accent can't be read.
pub(super) const ACCENT_FALLBACK: COLORREF = rgb(0x3B, 0x82, 0xF6);
/// Title text color (near-white).
pub(super) const TITLE_COLOR: COLORREF = rgb(0xF5, 0xF6, 0xF8);
/// Subtitle text color (muted light gray).
pub(super) const SUBTITLE_COLOR: COLORREF = rgb(0xA6, 0xAD, 0xBD);
/// Body background (white).
pub(super) const WHITE: COLORREF = rgb(0xFF, 0xFF, 0xFF);
/// Footer background (light gray).
pub(super) const FOOTER_BG: COLORREF = rgb(0xF4, 0xF5, 0xF7);
/// Hairline separator color.
pub(super) const HAIRLINE: COLORREF = rgb(0xE2, 0xE4, 0xE9);
/// Body text color (near-black).
pub(super) const TEXT_DARK: COLORREF = rgb(0x1A, 0x1D, 0x24);
/// Secondary button border.
pub(super) const BTN_BORDER: COLORREF = rgb(0xCE, 0xD2, 0xDA);
/// Secondary button fill when pressed.
pub(super) const BTN_PRESSED: COLORREF = rgb(0xEA, 0xEC, 0xF0);

/// How far the pressed primary button is darkened from the accent.
pub(super) const PRESSED_SCALE: f32 = 0.85;

/// Builds a `COLORREF` from RGB components.
///
/// # Arguments
///
/// * `r` - red channel.
/// * `g` - green channel.
/// * `b` - blue channel.
///
/// # Returns
///
/// The packed `0x00BBGGRR` color.
pub(super) const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

/// Reads the user's system accent color, falling back to a default blue.
///
/// # Returns
///
/// The accent as a `COLORREF`, or [`ACCENT_FALLBACK`] when the lookup fails.
pub(super) fn system_accent() -> COLORREF {
    let mut argb: u32 = 0;
    let mut opaque: i32 = 0;
    let result = unsafe { DwmGetColorizationColor(&mut argb, &mut opaque) };
    if result < 0 {
        return ACCENT_FALLBACK;
    }

    // The value is `0xAARRGGBB`.
    rgb(
        ((argb >> 16) & 0xFF) as u8,
        ((argb >> 8) & 0xFF) as u8,
        (argb & 0xFF) as u8,
    )
}

/// Scales a color's brightness.
///
/// # Arguments
///
/// * `color` - the color to scale.
/// * `factor` - the brightness multiplier, `0.0` (black) to `1.0` (unchanged).
///
/// # Returns
///
/// The scaled color.
pub(super) fn scale(color: COLORREF, factor: f32) -> COLORREF {
    let component = |shift: u32| (((color >> shift) & 0xFF) as f32 * factor).min(255.0) as u8;
    rgb(component(0), component(8), component(16))
}

/// Header title font face.
pub(super) const TITLE_FONT_NAME: &str = "Segoe UI Variable Display";
/// Header title font height (negative = character height in logical units).
pub(super) const TITLE_FONT_HEIGHT: i32 = -22;
/// Subtitle font face, shown under the title.
pub(super) const SUBTITLE_FONT_NAME: &str = "Segoe UI Variable Text";
/// Subtitle font height.
pub(super) const SUBTITLE_FONT_HEIGHT: i32 = -16;
/// Monospace font face for the report box.
pub(super) const MONO_FONT_NAME: &str = "Cascadia Mono";
/// Monospace font height.
pub(super) const MONO_FONT_HEIGHT: i32 = -15;
/// Button label font face.
pub(super) const BUTTON_FONT_NAME: &str = "Segoe UI Variable Text";
/// Button label font height.
pub(super) const BUTTON_FONT_HEIGHT: i32 = -15;

/// Creates a TrueType font, falling back to a system default for an unknown face.
///
/// # Arguments
///
/// * `face` - the font family name.
/// * `height` - the negative character height, in logical units.
/// * `semibold` - whether to use a semibold weight.
///
/// # Returns
///
/// The created font handle.
pub(super) unsafe fn create_font(face: &str, height: i32, semibold: bool) -> HFONT {
    let face = string_to_u16_buffer(face);
    let weight = if semibold { FW_SEMIBOLD } else { FW_NORMAL };
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_TT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH | FF_DONTCARE,
            face.as_ptr(),
        )
    }
}
