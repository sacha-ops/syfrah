//! TLS configuration for the peering protocol.
//!
//! Generates a self-signed certificate at runtime for the peering server.
//! The joiner connects with TLS but skips cert verification (TOFU).
//! The PIN provides the authentication layer — TLS provides encryption.

use std::sync::Arc;

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use syfrah_core::error::SyfrahError;

/// Ensure the rustls crypto provider is installed.
fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Generate a self-signed TLS certificate and private key.
pub fn generate_self_signed() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), SyfrahError> {
    ensure_crypto_provider();
    let cert = rcgen::generate_simple_self_signed(vec!["syfrah-peering".into()])
        .map_err(|e| SyfrahError::internal(format!("TLS cert generation failed: {e}")))?;

    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivateKeyDer::try_from(cert.key_pair.serialize_der())
        .map_err(|e| SyfrahError::internal(format!("TLS key conversion failed: {e}")))?;

    Ok((vec![cert_der], key_der))
}

/// Build a TLS server config with self-signed cert.
pub fn server_config() -> Result<Arc<rustls::ServerConfig>, SyfrahError> {
    let (certs, key) = generate_self_signed()?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| SyfrahError::internal(format!("TLS server config failed: {e}")))?;

    Ok(Arc::new(config))
}

/// Build a TLS client config that accepts any cert (TOFU).
pub fn client_config() -> Arc<rustls::ClientConfig> {
    ensure_crypto_provider();
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();

    Arc::new(config)
}

/// Certificate verifier that accepts everything (TOFU model).
/// Security comes from the PIN, not the cert.
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_cert() {
        let (certs, _key) = generate_self_signed().unwrap();
        assert_eq!(certs.len(), 1);
    }

    #[test]
    fn server_config_works() {
        let _ = server_config().unwrap();
    }

    #[test]
    fn client_config_works() {
        let _ = client_config();
    }
}
