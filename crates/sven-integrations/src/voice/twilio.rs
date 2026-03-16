// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Twilio voice call provider.
//!
//! Uses the Twilio REST API to place outbound calls with TwiML scripts.
//!
//! # Configuration
//! ```yaml
//! tools:
//!   voice:
//!     call_provider: twilio
//!     twilio_account_sid: "${TWILIO_SID}"
//!     twilio_auth_token: "${TWILIO_TOKEN}"
//!     twilio_phone_number: "+1234567890"
//!     webhook_base_url: "https://myagent.example.com"
//! ```

use async_trait::async_trait;
use serde::Deserialize;
use tracing::{debug, info};

use super::{CallParams, CallSummary, VoiceCallProvider};

/// Twilio voice call provider.
///
/// Places outbound calls using a TwiML `<Say>` verb to speak the script.
pub struct TwilioCallProvider {
    account_sid: String,
    auth_token: String,
    from_number: String,
    /// Public base URL for webhook callbacks (e.g. `https://myagent.example.com`).
    /// If absent, a static TwiML URL is used instead.
    webhook_base_url: Option<String>,
    client: reqwest::Client,
}

impl TwilioCallProvider {
    /// Create a new Twilio call provider.
    pub fn new(
        account_sid: impl Into<String>,
        auth_token: impl Into<String>,
        from_number: impl Into<String>,
        webhook_base_url: Option<String>,
    ) -> Self {
        Self {
            account_sid: account_sid.into(),
            auth_token: auth_token.into(),
            from_number: from_number.into(),
            webhook_base_url,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("Twilio HTTP client"),
        }
    }

    fn calls_url(&self) -> String {
        format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Calls.json",
            self.account_sid
        )
    }
}

#[derive(Debug, Deserialize)]
struct TwilioCallResponse {
    sid: Option<String>,
    status: Option<String>,
}

#[async_trait]
impl VoiceCallProvider for TwilioCallProvider {
    async fn call(&self, params: &CallParams) -> anyhow::Result<CallSummary> {
        info!(to = %params.to, "Twilio: initiating call");
        debug!(script_chars = params.script.len(), "Twilio: call script");

        // Build inline TwiML that speaks the script
        let twiml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<Response>
  <Say voice="alice">{}</Say>
</Response>"#,
            xml_escape(&params.script)
        );

        let encoded_twiml = urlencoding(&twiml);
        let twiml_bin_url = format!("http://twimlets.com/echo?Twiml={}", encoded_twiml);

        // Use webhook if configured, otherwise fall back to inline TwiML via twimlets
        let twiml_url = self
            .webhook_base_url
            .as_deref()
            .map(|base| format!("{base}/voice/twiml"))
            .unwrap_or(twiml_bin_url);

        let timeout = params.timeout_secs.unwrap_or(30);

        let form = [
            ("To", params.to.as_str()),
            ("From", self.from_number.as_str()),
            ("Url", twiml_url.as_str()),
            ("Timeout", &timeout.to_string()),
            ("StatusCallback", ""),
        ];

        let resp: TwilioCallResponse = self
            .client
            .post(self.calls_url())
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&form)
            .send()
            .await?
            .json()
            .await?;

        Ok(CallSummary {
            call_id: resp.sid.unwrap_or_else(|| "unknown".to_string()),
            to: params.to.clone(),
            status: resp.status.unwrap_or_else(|| "queued".to_string()),
            duration_secs: 0,
            transcript: None,
        })
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect()
}
