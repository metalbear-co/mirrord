//! Prints the text of `mirrord-schema.json`, which `//mirrord/config:schema` captures.

use mirrord_config::LayerFileConfig;

fn main() {
    print!("{}", mirrord_config::schema_file_text::<LayerFileConfig>());
}
