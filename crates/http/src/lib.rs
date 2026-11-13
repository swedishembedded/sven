// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for lean, many-process agent
// deployments for its clients. If your team needs expertise in keeping
// networked Rust services small then you can procure our services by sending
// an email to info@swedishembedded.com.
//! The one way sven builds HTTP clients.
//!
//! TLS is rustls with its `ring` provider and nothing else: reqwest is built
//! without a provider of its own (`rustls-no-provider`), so the binary links
//! one crypto library rather than two, and every process installs `ring` as
//! its provider before the first client is built. [`client_builder`] does
//! that for sven's own clients; [`ensure_crypto_provider`] is for code that
//! has a library build clients itself (rmcp's sign-in, for one), and is
//! called before it.

#[cfg(not(target_arch = "wasm32"))]
pub mod egress;

/// Installs `ring` as this process's TLS provider, once; later calls, and a
/// provider installed by someone else first, leave it as it is. Nothing to do
/// in a browser, where requests are the page's own `fetch`.
pub fn ensure_crypto_provider() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }
}

/// A client builder whose TLS provider is in place: the start of every HTTP
/// client sven builds, enforcing the installed [`egress`] policy (none by
/// default).
pub fn client_builder() -> reqwest::ClientBuilder {
    client_builder_resolving(None)
}

/// [`client_builder`], with `inner` resolving the names the egress policy
/// does not map: for a caller that restricts resolution further itself.
#[cfg(not(target_arch = "wasm32"))]
pub fn client_builder_resolving(
    inner: Option<std::sync::Arc<dyn reqwest::dns::Resolve>>,
) -> reqwest::ClientBuilder {
    match egress::installed() {
        Some(policy) => policy.apply(model_client_builder(), inner),
        None => model_client_builder(),
    }
}

/// In a browser requests are the page's own `fetch`; there is no policy.
#[cfg(target_arch = "wasm32")]
pub fn client_builder_resolving(_inner: Option<()>) -> reqwest::ClientBuilder {
    model_client_builder()
}

/// A client builder that ignores the egress policy: only for the model
/// provider's endpoint, which is configured separately (`providers`,
/// `model`). Everything else uses [`client_builder`].
pub fn model_client_builder() -> reqwest::ClientBuilder {
    ensure_crypto_provider();
    reqwest::Client::builder()
}

/// An error and its causes as one line (`a: b: c`): reqwest's own text for a
/// failed request hides why (a refused destination, a failed lookup).
#[must_use]
pub fn describe(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        let part = next.to_string();
        if !text.contains(&part) {
            text.push_str(": ");
            text.push_str(&part);
        }
        cause = next.source();
    }
    text
}

/// A client with reqwest's defaults and the TLS provider in place.
///
/// # Panics
///
/// Never in practice: building a client with no settings of its own fails
/// only when TLS cannot be set up at all, which the provider rules out.
#[must_use]
pub fn client() -> reqwest::Client {
    client_builder()
        .build()
        .expect("an HTTP client with default settings builds")
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    /// A client builds with no provider installed beforehand, as in a test
    /// or a program using sven's crates as a library.
    #[test]
    fn a_client_builds_without_anyone_installing_a_provider_first() {
        let _client = super::client();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
