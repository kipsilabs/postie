//! rustls client configuration (ring provider), with an opt-out of certificate verification for
//! the `insecure_ssl` setting.

use std::sync::{Arc, OnceLock};

use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn provider() -> Arc<CryptoProvider> {
    static P: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    P.get_or_init(|| Arc::new(rustls::crypto::ring::default_provider()))
        .clone()
}

/// A shared client config: verifying against the Mozilla roots, or not verifying at all.
pub fn client_config(insecure: bool) -> Arc<ClientConfig> {
    static SECURE: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    static INSECURE: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    let build = |insecure: bool| {
        let p = provider();
        let b = ClientConfig::builder_with_provider(p.clone())
            .with_safe_default_protocol_versions()
            .expect("ring supports TLS 1.2/1.3");
        let cfg = if insecure {
            b.dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify(p)))
                .with_no_client_auth()
        } else {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            b.with_root_certificates(roots).with_no_client_auth()
        };
        Arc::new(cfg)
    };
    if insecure {
        INSECURE.get_or_init(|| build(true)).clone()
    } else {
        SECURE.get_or_init(|| build(false)).clone()
    }
}
