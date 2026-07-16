Removed the `--expand` flag from the `cargo shear` CI job. This means check are performed statically
and do not require full compilation. Also ignored crates used by macro expansions.
