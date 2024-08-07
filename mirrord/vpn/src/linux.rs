use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub struct ResolvOverride {
    path: PathBuf,
    original_file: PathBuf,
}

impl ResolvOverride {
    pub async fn accuire_override<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let path = PathBuf::from(path.as_ref());
        let original_file = {
            let mut path = path.clone();

            path.set_file_name(match path.file_name().and_then(OsStr::to_str) {
                Some(filename) => format!("{filename}.backup"),
                None => "resolv.conf.backup".to_string(),
            });

            path
        };

        tokio::fs::copy(&path, &original_file).await?;

        Ok(ResolvOverride {
            path,
            original_file,
        })
    }

    pub async fn update_resolv(&self, bytes: &[u8]) -> std::io::Result<()> {
        tokio::fs::write(&self.path, bytes).await?;

        Ok(())
    }

    pub async fn unmount(self) -> std::io::Result<()> {
        let ResolvOverride {
            path,
            original_file,
        } = self;

        tokio::fs::copy(&original_file, &path).await?;
        tokio::fs::remove_file(&original_file).await?;

        Ok(())
    }
}
