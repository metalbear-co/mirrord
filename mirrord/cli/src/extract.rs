/// Extract to given directory, or tmp by default.
/// If prefix is true, add a random prefix to the file name that identifies the specific build
/// of the layer. This is useful for debug purposes usually.
fn extract_library(
    dest_dir: Option<String>,
    progress: &TaskProgress,
    prefix: bool,
) -> Result<PathBuf> {
    let progress = progress.subtask("extracting layer");
    let extension = Path::new(env!("MIRRORD_LAYER_FILE"))
        .extension()
        .unwrap()
        .to_str()
        .unwrap();

    let file_name = if prefix {
        format!("{}-libmirrord_layer.{extension}", const_random!(u64))
    } else {
        format!("libmirrord_layer.{extension}")
    };

    let file_path = match dest_dir {
        Some(dest_dir) => std::path::Path::new(&dest_dir).join(file_name),
        None => temp_dir().as_path().join(file_name),
    };
    if !file_path.exists() {
        let mut file = File::create(&file_path)
            .into_diagnostic()
            .wrap_err_with(|| format!("Path \"{}\" creation failed", file_path.display()))?;
        let bytes = include_bytes!(env!("MIRRORD_LAYER_FILE"));
        file.write_all(bytes).unwrap();
        debug!("Extracted library file to {:?}", &file_path);
    }

    progress.done_with("layer extracted");
    Ok(file_path)
}