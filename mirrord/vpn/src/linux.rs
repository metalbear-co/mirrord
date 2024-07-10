use std::{
    ffi::OsStr,
    io::Write,
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

    pub async fn update_resolv(&self, nameservers: &[String]) -> std::io::Result<()> {
        let mut updated_file = Vec::default();

        updated_file.write_all(b"search home\n")?;

        for nameserver in nameservers {
            updated_file.write_all(b"nameserver ")?;
            updated_file.write_all(nameserver.as_bytes())?;
            updated_file.write_all(b"\n")?;
        }

        tokio::fs::write(&self.path, updated_file).await?;

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
