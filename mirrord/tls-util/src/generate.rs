use rcgen::{
    BasicConstraints, CertificateParams, CertifiedKey, DnType, DnValue, Error, IsCa, Issuer,
    KeyPair, KeyUsagePurpose,
};

/// Generates a new [`CertifiedKey`] with a random [`KeyPair`].
///
/// # Params
///
/// * `name` - will be used as the subject alternate name, must be either an IP address or a DNS
///   name.
/// * `issuer` - optional issuer for this certificate. Pass [`None`] to generate a self-signed
///   certificate.
/// * `can_sign_others` - whether the generated certificate should be allowed to sign other
///   certificates. Mind that such certificate cannot be used as an end-entity.
pub fn generate_cert(
    name: &str,
    issuer: Option<&CertifiedKey<KeyPair>>,
    can_sign_others: bool,
) -> Result<CertifiedKey<KeyPair>, Error> {
    let signing_key = KeyPair::generate()?;

    let mut params = CertificateParams::new(vec![name.to_owned()])?;
    params
        .distinguished_name
        .push(DnType::CommonName, DnValue::Utf8String(name.to_owned()));

    if can_sign_others {
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    }

    let cert = match issuer {
        Some(issuer) => {
            let issuer = Issuer::from_ca_cert_der(issuer.cert.der(), &issuer.signing_key)?;
            params.signed_by(&signing_key, &issuer)?
        }
        None => params.self_signed(&signing_key)?,
    };

    Ok(CertifiedKey { cert, signing_key })
}
