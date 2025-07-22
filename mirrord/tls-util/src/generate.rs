use rcgen::{
    BasicConstraints, CertificateParams, CertifiedKey, DnType, DnValue, Error, IsCa, KeyPair,
    KeyUsagePurpose,
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
    issuer: Option<&CertifiedKey>,
    can_sign_others: bool,
) -> Result<CertifiedKey, Error> {
    let key_pair = KeyPair::generate()?;

    let mut params = CertificateParams::new(vec![name.to_string()])?;
    params
        .distinguished_name
        .push(DnType::CommonName, DnValue::Utf8String(name.to_string()));

    if can_sign_others {
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    }

    let cert = match issuer {
        Some(issuer) => params.signed_by(&key_pair, &issuer.cert, &issuer.key_pair)?,
        None => params.self_signed(&key_pair)?,
    };

    Ok(CertifiedKey { cert, key_pair })
}
