//! Prints the text of `mirrord-up-schema.json`, which `//mirrord/up:schema` captures.

use mirrord_up::UpConfig;

fn main() {
    print!("{}", mirrord_config::schema_file_text::<UpConfig>());
}
