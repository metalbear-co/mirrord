use std::{fs::File, io::BufReader};

use rustls::pki_types::CertificateDer;
use rustls_pemfile::Item;

use crate::{NicePath, TlsUtilError};

pub struct Certs<'a, P> {
    path: &'a P,
    error_encountered: bool,
    file: Option<BufReader<File>>,
}

impl<'a, P: NicePath> Certs<'a, P> {
    pub fn read(path: &'a P) -> Self {
        Self {
            path,
            error_encountered: false,
            file: None,
        }
    }
}

impl<P: NicePath> Iterator for Certs<'_, P> {
    type Item = Result<CertificateDer<'static>, TlsUtilError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.error_encountered {
            return None;
        }

        let mut file = match self.file.take() {
            Some(file) => file,
            None => match File::open(self.path.real_path()) {
                Ok(file) => BufReader::new(file),
                Err(error) => {
                    self.error_encountered = true;
                    return Some(Err(TlsUtilError::ParsePemFileError {
                        error,
                        path: self.path.display_path().to_path_buf(),
                    }));
                }
            },
        };

        loop {
            match rustls_pemfile::read_one(&mut file).transpose()? {
                Ok(Item::X509Certificate(cert)) => {
                    self.file.replace(file);
                    break Some(Ok(cert));
                }
                Ok(..) => {}
                Err(error) => {
                    self.error_encountered = true;
                    break Some(Err(TlsUtilError::ParsePemFileError {
                        error,
                        path: self.path.display_path().to_path_buf(),
                    }));
                }
            }
        }
    }
}
