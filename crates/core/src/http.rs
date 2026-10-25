//! Shared HTTP client (D21): rustls + ring + webpki-roots, never the platform verifier.

use std::sync::Arc;
use std::time::Duration;

use crate::error::Result;

/// Install ring as the process-wide rustls provider (idempotent, harmless if already set).
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// rustls client config with the webpki root store (also used for wss:// relays).
pub fn tls_config() -> rustls::ClientConfig {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("ring supports default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

/// reqwest client with our TLS config and a timeout.
pub fn client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("magnetar/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(10))
        .tls_backend_preconfigured(tls_config())
        .build()?)
}
