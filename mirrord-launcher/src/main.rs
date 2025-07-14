mod handle;
mod launcher;
mod process;
mod registry;
mod win_str;

use core::time;
use std::path::Path;

use crate::process::{Suspended, create_process};
use launcher::ifeo::start_ifeo;

const NOTEPAD: &str = r#"c:\windows\notepad.exe"#;
const PAINT: &str = r#"mspaint.exe"#;

#[inline]
fn start_notepad() {
    let _ = create_process(NOTEPAD, [], Suspended::No);
}

fn main() {
    let success = start_ifeo(Path::new(NOTEPAD), Path::new(PAINT), [], Suspended::No);
    start_notepad();

    if success.is_some() {
        println!("success!");
    }
}
