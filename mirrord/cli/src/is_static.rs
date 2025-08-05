use std::path::Path;

use xmas_elf::{ElfFile, program::Type};

/// Returns true if we can be certain that the binary under the given path is statically linked.
///
/// Might yield false negatives.
///
/// Note that mirrord *can* possibly be used with static binaries - when the actual application is a
/// child process of the target binary, and it is dynamically linked.
/// Therefore, static linking is *not* and indication for a hard fail.
pub fn is_binary_static(binary_path: &Path) -> bool {
    let content = match std::fs::read(binary_path) {
        Ok(content) => content,
        Err(error) => {
            tracing::warn!(
                %error,
                binary_path = %binary_path.display(),
                "Failed to read the file while checking if the binary is statically linked",
            );
            return false;
        }
    };

    let elf = match ElfFile::new(&content) {
        Ok(elf) => elf,
        Err(error) => {
            tracing::warn!(
                error,
                binary_path = %binary_path.display(),
                "Failed to parse ELF file while checking if the binary is statically linked",
            );
            return false;
        }
    };

    for header in elf.program_iter() {
        if let Ok(Type::Dynamic) = header.get_type() {
            return false;
        }
    }

    true
}
