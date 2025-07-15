mod handle;
mod launcher;
mod process;
mod registry;

use std::io::Result;

use crate::process::{Suspended, create_process};
use launcher::ifeo::start_ifeo;

const NOTEPAD: &str = r#"c:\windows\notepad.exe"#;
const PAINT: &str = r#"mspaint.exe"#;

#[inline]
fn start_notepad() {
    let _ = create_process(NOTEPAD, [], Suspended::No);
}

fn main() -> Result<()> {
    let _ = start_ifeo(NOTEPAD, PAINT, [], Suspended::No)?;
    println!("success!");
    start_notepad();

    Ok(())
}
