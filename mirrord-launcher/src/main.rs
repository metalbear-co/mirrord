mod launcher;
mod process;
mod registry;
mod win_str;

use std::path::Path;

use launcher::ifeo::{remove_ifeo, set_ifeo, start_ifeo};

#[inline]
fn start_notepad() {
    let _ = std::process::Command::new("notepad.exe").spawn();
}

fn main() {
    const NOTEPAD: &str = r#"c:\windows\notepad.exe"#;
    const PAINT: &str = r#"mspaint.exe"#;

    let success = start_ifeo(Path::new(NOTEPAD), Path::new(PAINT));
    start_notepad();

    if success {
        println!("success!");
    }
}
