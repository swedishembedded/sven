// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! zbus proxy for brain's `com.swedishembedded.Brain1.Manager` interface.

use std::collections::HashMap;

use zbus::zvariant::OwnedFd;

/// Well-known bus name brain registers.
pub const DEFAULT_SERVICE: &str = "com.swedishembedded.Brain1";
/// Object path the manager is served at.
pub const DEFAULT_PATH: &str = "/com/swedishembedded/Brain1";
/// Interface name of the manager.
pub const INTERFACE: &str = "com.swedishembedded.Brain1.Manager";

/// brain's manager interface.
///
/// Wire signature of `Run` is `sssa{sh}ss → sa{sh}s`:
///
/// | direction | field       | meaning                                          |
/// |-----------|-------------|--------------------------------------------------|
/// | in        | `model`     | model id, e.g. `brain/omni`                      |
/// | in        | `action`    | action name, e.g. `generate`                     |
/// | in        | `params`    | JSON **object** string of action parameters      |
/// | in        | `in_fds`    | blob name → fd carrying that blob's raw payload  |
/// | in        | `in_meta`   | JSON object string, blob name → blob metadata    |
/// | in        | `transport` | transport tag, e.g. `memfd`                      |
/// | out       | `result`    | JSON object string                               |
/// | out       | `out_fds`   | blob name → fd (`text` carries generated output) |
/// | out       | `out_meta`  | JSON object string of output blob metadata       |
///
/// Only the one-shot `Run` is modelled here — see the module docs of
/// [`crate::dbus`] for why streaming `Subscribe` is deliberately out of scope.
#[zbus::proxy(
    interface = "com.swedishembedded.Brain1.Manager",
    default_service = "com.swedishembedded.Brain1",
    default_path = "/com/swedishembedded/Brain1"
)]
pub trait Manager {
    /// Run one action against one model.
    fn run(
        &self,
        model: &str,
        action: &str,
        params: &str,
        in_fds: HashMap<String, OwnedFd>,
        in_meta: &str,
        transport: &str,
    ) -> zbus::Result<(String, HashMap<String, OwnedFd>, String)>;

    /// List the model ids the server currently has loaded or can load.
    ///
    /// Assumed to be an array of strings (`as`).  Because a mismatch here is
    /// only discovered at runtime and this call is purely informational, the
    /// provider treats any failure as "unknown" rather than propagating it.
    fn list_models(&self) -> zbus::Result<Vec<String>>;

    /// Full model manifests as a JSON string.
    fn manifests(&self) -> zbus::Result<String>;

    /// Server version string.
    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;
}
