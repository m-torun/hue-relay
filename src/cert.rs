//! The relay's HTTPS certificate: self-signed, CN = bridge id, the form of bridges from before 2019.
//! At least one Hue Sync client rejects a chain to a made-up CA (TLS alert unknown_ca) but accepts this.

use std::path::{Path, PathBuf};

use anyhow::Result;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose, SerialNumber,
};
use time::{Date, Month, PrimitiveDateTime, Time};

/// The certificate and key files in the data folder, made once and then kept, so clients that
/// remember the certificate keep seeing the same one.
pub fn ensure(data_dir: &Path, bridge_id: &str) -> Result<(PathBuf, PathBuf)> {
    let cert = data_dir.join("cert-selfsigned.pem");
    let key = data_dir.join("key-selfsigned.pem");
    if cert.exists() && key.exists() {
        return Ok((cert, key));
    }
    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CountryName, "NL");
    dn.push(DnType::OrganizationName, "Philips Hue");
    dn.push(DnType::CommonName, bridge_id.to_lowercase());
    params.distinguished_name = dn;
    params.serial_number = Some(SerialNumber::from_slice(&hex::decode(bridge_id)?));
    let at = |y, m, d| PrimitiveDateTime::new(Date::from_calendar_date(y, m, d).unwrap(), Time::MIDNIGHT).assume_utc();
    params.not_before = at(2017, Month::January, 1);
    params.not_after = at(2038, Month::January, 19);
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    let kp = KeyPair::generate()?;
    let c = params.self_signed(&kp)?;
    std::fs::write(&cert, c.pem())?;
    std::fs::write(&key, kp.serialize_pem())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok((cert, key))
}
