use std::path::Path;

use crate::hooks::files::util::remove_root_dir_from_path;

#[test]
fn try_get_linux_path() {
    const WINDOWS_PATH: &str = r#"\??\C:\home\gabrielaelae\dev\MIRRORD\mirrord\target\debug"#;
    const LINUX_PATH: &str = r#"/home/gabrielaelae/dev/MIRRORD/mirrord/target/debug"#;

    let new_path = remove_root_dir_from_path(WINDOWS_PATH);
    assert!(&new_path.is_some());
    assert_eq!(Path::new(&new_path.unwrap()), Path::new(LINUX_PATH));
}