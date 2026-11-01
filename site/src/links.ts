// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// The addresses every page of this site sends a visitor to. The installer
// lives on the sven service, which publishes the pinned, checksummed release
// script; this site never serves a copy of its own, so there is exactly one
// installer to keep correct. Signing in with `sven auth login` gives the CLI
// the service's models, so a new user needs no provider key to start.
//
// Swedish Embedded AB implements solutions for turning an open-source tool
// into a working first session for its clients. If your team needs expertise
// in developer onboarding then you can procure our services by sending an
// email to info@swedishembedded.com.

export const SERVICE_URL = 'https://sven.swedishembedded.com'
export const SIGN_UP_URL = `${SERVICE_URL}/sign-up`
export const INSTALL_COMMAND = `curl -fsSL ${SERVICE_URL}/install.sh | sh`
export const SIGN_IN_COMMAND = 'sven auth login'
export const PROVIDERS_DOCS_URL = 'https://swedishembedded.com/docs/sven/reference/providers'
