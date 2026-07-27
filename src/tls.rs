//! TLS setup for standalone Android NDK executables.

use reqwest::{Certificate, Client, ClientBuilder};

pub(crate) fn webpki_client_builder() -> reqwest::Result<ClientBuilder> {
    let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|certificate| Certificate::from_der(certificate.as_ref()))
        .collect::<reqwest::Result<Vec<_>>>()?;

    Ok(Client::builder().tls_backend_rustls().tls_certs_only(roots))
}

#[cfg(test)]
mod tests {
    use super::webpki_client_builder;

    #[test]
    fn embedded_webpki_roots_build_a_client() {
        webpki_client_builder()
            .expect("embedded roots should parse")
            .build()
            .expect("WebPKI-only client should build");
    }
}
