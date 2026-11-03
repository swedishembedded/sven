// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for signing command-line tools in
// to hosted services without a secret ever passing through a browser page, for
// its clients. If your team needs expertise in OAuth for native applications
// (RFC 7636, RFC 8252) then you can procure our services by sending an email
// to info@swedishembedded.com.
//! The secrets and the request of an authorization code grant with PKCE
//! (RFC 7636): what one sign-in attempt invents, and the address the person
//! is sent to.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::RngCore as _;
use sha2::{Digest as _, Sha256};

/// Random bytes behind the verifier and the state: base64url makes the
/// verifier 43 characters, the RFC's minimum.
const SECRET_BYTES: usize = 32;

/// What one sign-in attempt invents. The verifier is a secret of this
/// process: only its hash goes into the browser address, the verifier itself
/// only to the token endpoint.
pub(super) struct Pkce {
    verifier: String,
    /// BASE64URL(SHA256(verifier)), what the authorization request carries.
    pub(super) challenge: String,
    /// Binds the browser's return to this attempt.
    pub(super) state: String,
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"<redacted>")
            .field("challenge", &self.challenge)
            .field("state", &self.state)
            .finish()
    }
}

impl Pkce {
    pub(super) fn generate() -> Self {
        Self::from_secrets(random_token(), random_token())
    }

    fn from_secrets(verifier: String, state: String) -> Self {
        Self {
            challenge: challenge_of(&verifier),
            verifier,
            state,
        }
    }

    pub(super) fn verifier(&self) -> &str {
        &self.verifier
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; SECRET_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The `S256` code challenge of `verifier` (RFC 7636 section 4.2).
pub(super) fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// What the authorization request says besides the endpoint.
pub(super) struct AuthorizeRequest<'a> {
    pub redirect_uri: &'a str,
    pub state: &'a str,
    pub code_challenge: &'a str,
    /// What the server should call this machine.
    pub name: &'a str,
}

/// The address to send the person to: `endpoint` with the request's
/// parameters, every value percent-encoded.
pub(super) fn authorize_url(endpoint: &str, request: &AuthorizeRequest<'_>) -> String {
    let query = [
        ("response_type", "code"),
        ("client_id", "sven"),
        ("redirect_uri", request.redirect_uri),
        ("state", request.state),
        ("code_challenge", request.code_challenge),
        ("code_challenge_method", "S256"),
        ("name", request.name),
    ]
    .map(|(key, value)| format!("{key}={}", percent_encode(value)))
    .join("&");
    let joiner = if endpoint.contains('?') { '&' } else { '?' };
    format!("{endpoint}{joiner}{query}")
}

/// Everything but RFC 3986 unreserved characters as `%HH`.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 appendix B.
    #[test]
    fn the_challenge_is_the_rfcs_for_its_test_vector() {
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_secrets_are_rfc_verifiers_and_never_repeat() {
        let (a, b) = (Pkce::generate(), Pkce::generate());
        for pkce in [&a, &b] {
            assert_eq!(pkce.verifier().len(), 43);
            assert!(pkce
                .verifier()
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')));
            assert_eq!(pkce.challenge, challenge_of(pkce.verifier()));
            assert_eq!(pkce.state.len(), 43);
            assert!(!format!("{pkce:?}").contains(pkce.verifier()));
        }
        assert_ne!(a.verifier(), b.verifier());
        assert_ne!(a.state, b.state);
    }

    #[test]
    fn the_authorize_url_carries_every_parameter_encoded() {
        let url = authorize_url(
            "https://s/connect/sven/authorize",
            &AuthorizeRequest {
                redirect_uri: "http://127.0.0.1:5000/callback",
                state: "st-ate_1",
                code_challenge: "E9Melhoa2OwvRFlwiXZCAYnx2B9_Gs3LiJCNKmkCZ_I",
                name: "build box&co",
            },
        );
        assert_eq!(
            url,
            "https://s/connect/sven/authorize?response_type=code&client_id=sven\
             &redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2Fcallback&state=st-ate_1\
             &code_challenge=E9Melhoa2OwvRFlwiXZCAYnx2B9_Gs3LiJCNKmkCZ_I\
             &code_challenge_method=S256&name=build%20box%26co"
        );
        let with_query = authorize_url(
            "https://s/a?tenant=1",
            &AuthorizeRequest {
                redirect_uri: "r",
                state: "s",
                code_challenge: "c",
                name: "n",
            },
        );
        assert!(with_query.starts_with("https://s/a?tenant=1&response_type=code"));
    }
}
