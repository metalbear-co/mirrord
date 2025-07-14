mod launcher;
mod registry;
mod win_str;

use launcher::ifeo::{remove_ifeo, set_ifeo};

#[inline]
fn start_notepad() {
    let _ = std::process::Command::new("notepad.exe").spawn();
}

fn main() {
    let set = set_ifeo("notepad.exe", "mspaint.exe");
    start_notepad();
    let del = remove_ifeo("notepad.exe");
    start_notepad();

    if set && del {
        println!("success!");
    }
}
