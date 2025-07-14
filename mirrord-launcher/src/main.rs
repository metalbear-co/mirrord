mod handle;
mod launcher;
mod process;
mod registry;
mod win_str;

use core::time;
use std::path::Path;

use launcher::ifeo::start_ifeo;
use crate::process::create_process;

const NOTEPAD: &str = r#"c:\windows\notepad.exe"#;
const PAINT: &str = r#"mspaint.exe"#;

#[inline]
fn start_notepad() {
    let _ = create_process(NOTEPAD, [], false);
}

fn main() {
    let success = start_ifeo(Path::new(NOTEPAD), Path::new(PAINT));
    start_notepad();

    if success.is_some() {
        println!("success!");
    }
}
