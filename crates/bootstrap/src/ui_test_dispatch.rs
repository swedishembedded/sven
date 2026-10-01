// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Runs one [`sven_machines::UiTestMachine`] step to completion for the
//! agent-dispatch stdio contract (`mode: "ui-test"`).
//!
//! [`dispatch_ui_test_step`] is the one function a `sven` CLI subcommand
//! needs: it builds the machine through the SAME [`RuntimeBuilder`]/
//! [`sven_machines::ModeRegistry`] path every other sven machine already
//! uses, wires the REAL `sven-tools-android`/`sven-tools-agent` tool
//! implementations by default, and reports the run's outcome as a plain
//! `Result<Value, String>` - exactly the shape a CLI wrapper turns into the
//! `{"ok": true, "output": ...}` / `{"ok": false, "error": ...}` reply.
//!
//! Handles exactly one instruction per call, matching the per-node dispatch
//! granularity an orchestrating host works in: every other top-level
//! `params` field (a value the host resolved from an upstream node) is
//! seeded into `UiTestMachine`'s existing variable-binding mechanism
//! (`UiTestScript::vars`, `ui_test/vars.rs`) before the one step compiles,
//! so an upstream node's output reaches this step's `value_ref` resolution
//! without any new sven-side plumbing.
//!
//! Swedish Embedded AB implements solutions for deterministic, CI-dispatched
//! Android UI test automation for its clients. If your team needs expertise
//! in HSM-driven agent dispatch, device-farm orchestration, or vision-model-
//! backed UI testing, you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::Config;
use sven_executors::ToolExecutor;
use sven_hsm::Event;
use sven_machines::machines::ui_test::{ask_user_binding, ERROR_FACT, RESULTS_FACT};
use sven_tool_registry::{Tool, ToolRegistry};
use sven_tools_android::adb::{self, DeviceLister, RealDeviceLister, SerialPick};

use crate::runtime_builder::{RuntimeBuilder, ToolExecutorFactory};
use crate::session_handles::HumanGateResponder;

/// The device the dispatching host resolved and leased for this step, or
/// `None` when the node declared no device requirement.
///
/// `device_id` is the host's stable, logical catalog/leasing key (e.g.
/// `"phone-1"`) - it names a ROLE in the host's own placement/leasing
/// bookkeeping, not a physical device, and is never a valid ADB `-s`
/// argument. `serial` is the actual physical address (`adb devices`' first
/// column, e.g. `"ec677a50"`) - the two are deliberately never conflated: a
/// device could be re-plugged under the same logical role with a different
/// physical unit over time, and the host's own catalog may legitimately not
/// know the serial at all (see `resolve_effective_serial`'s own doc for what
/// happens then).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiTestDevice {
    pub provider_id: String,
    pub device_id: String,
    /// The real ADB serial, when the host's catalog reported one. `None` is
    /// a real, expected state (not every catalog entry names a serial) - see
    /// `resolve_effective_serial`'s own doc for how that degrades to
    /// ordinary auto-detect rather than a hard failure.
    pub serial: Option<String>,
    /// The packages the host declares are installed on this device.
    ///
    /// Empty is a real, expected state - a host that keeps no app inventory,
    /// or a device with nothing installed yet - and simply means a loose
    /// app-name hint in a step gets resolved by asking the device instead.
    /// When it IS populated, a hint resolves against this list first, which
    /// makes the answer exact (the host placed this run on the device and
    /// knows what it runs) and costs no `pm list packages` round trip. See
    /// `sven_tools_android::adb::resolve_package_validated` for the exact
    /// precedence and why a declaration is trusted rather than re-verified.
    pub apps: Vec<String>,
}

/// Test-only (and otherwise advanced-use) seams: substitute the model
/// provider or any of the three tools [`dispatch_ui_test_step`] wires by
/// default. Every field defaults to `None`, which means "use the real
/// thing" - production callers pass [`UiTestDispatchOverrides::default`].
#[derive(Default)]
pub struct UiTestDispatchOverrides {
    /// Replaces the model provider the step compiler's bounded,
    /// schema-constrained `CallLlm` turn talks to. `None` builds the real
    /// provider from `config.model`.
    pub model_provider: Option<Box<dyn sven_model::ModelProvider>>,
    /// Replaces the real `android` tool (`sven_tools_android::AndroidTool`).
    pub android_tool: Option<Arc<dyn Tool>>,
    /// Replaces the real `ask_question` tool
    /// (`sven_tools_agent::AskQuestionTool::no_user`: a dispatched step has
    /// nobody to ask, so a step that needs a person - a value to enter, a
    /// secure screen to act on - fails at once, saying so).
    pub ask_tool: Option<Arc<dyn Tool>>,
    /// Replaces the real device lister
    /// (`sven_tools_android::adb::RealDeviceLister`) [`resolve_effective_serial`]
    /// uses to validate `device.serial` against what's actually attached.
    /// `None` uses the real one (shells out to `adb devices`) - a test
    /// substitutes a fake to exercise the fallback/ambiguity paths without a
    /// real device.
    pub device_lister: Option<Arc<dyn DeviceLister>>,
    /// Called once per DISTINCT state the step's machine passes through,
    /// with that state's label, while the step is still running.
    ///
    /// This is not a test seam like the fields above it. A dispatched step is
    /// otherwise a black box to whoever asked for it: the caller learns
    /// nothing at all between "started" and "finished", which for a real UI
    /// step is minutes. `RuntimeStatus` already publishes the machine's
    /// current state label on a watch channel; this only forwards it, so it
    /// costs one task and no new machinery.
    ///
    /// Distinct states only, not every status update: the watch channel
    /// republishes on every processed event, and a caller relaying that to a
    /// user would be sending hundreds of identical messages.
    ///
    /// `None` (the default) spawns nothing at all, so a caller that does not
    /// want progress pays nothing for the ability to have it.
    pub on_state: Option<StateReporter>,
    /// Who answers the kernel-level question and approval gates this step
    /// raises.
    ///
    /// `None` answers them at once
    /// ([`KernelChannels::answer_unattended`](crate::session_handles::KernelChannels::answer_unattended)):
    /// the hosts this entry was written for (CI, one-shot demos) have nobody
    /// to ask, and a step that waits for nobody helps none of them. A host
    /// with a person attached passes a responder, and then it is the host's
    /// business whether it asks, denies, or approves-and-records.
    pub on_human_gate: Option<HumanGateResponder>,
}

/// Where [`UiTestDispatchOverrides::on_state`] sends each state label.
///
/// A named type rather than the inline `Arc<dyn Fn(&str) + Send + Sync>`:
/// clippy asks for it at this nesting, and the name is the better
/// documentation anyway -- every call site now says what the callback is FOR
/// rather than restating its shape.
pub type StateReporter = Arc<dyn Fn(&str) + Send + Sync>;

/// Runs one `UiTestMachine` step to completion and returns its outcome.
///
/// `instruction` becomes the machine's one-element step list. Every other
/// top-level field of `params` is seeded into the machine's variable-binding
/// mechanism before the step compiles (see the module doc). `device`
/// selects which real Android device `sven-tools-android` drives - its
/// `serial` is validated against what is actually attached (see
/// `resolve_effective_serial`), falling back to `AndroidTool`'s own
/// `SVEN_ANDROID_SERIAL` env var (then auto-detection) when `device` is
/// `None`.
///
/// # Errors
///
/// Returns `Err` both for a genuine setup failure (couldn't build the
/// kernel session, couldn't join the runtime) and for an ordinary failed UI
/// step (the machine reached `Failed` and recorded a reason) - the caller
/// (the host's stdio contract) treats both the same way: a well-formed
/// `{"ok": false, "error": ...}` reply, never a process crash.
pub async fn dispatch_ui_test_step(
    config: Arc<Config>,
    device: Option<&UiTestDevice>,
    params: &Value,
    overrides: UiTestDispatchOverrides,
) -> Result<Value, String> {
    let instruction = params
        .get("instruction")
        .and_then(Value::as_str)
        .ok_or_else(|| "params.instruction is required and must be a string".to_string())?
        .to_string();

    let vars = extract_vars(params);
    let script = json!({ "steps": [instruction], "vars": vars }).to_string();

    let lister: Arc<dyn DeviceLister> = overrides
        .device_lister
        .unwrap_or_else(|| Arc::new(RealDeviceLister));
    let serial = resolve_effective_serial(device, lister.as_ref()).await?;

    let declared_packages = device.map(|d| d.apps.clone()).unwrap_or_default();
    let android_tool = overrides.android_tool.unwrap_or_else(|| {
        Arc::new(
            sven_tools_android::AndroidTool::new(serial).with_declared_packages(declared_packages),
        )
    });
    let ask_tool = overrides
        .ask_tool
        .unwrap_or_else(|| Arc::new(sven_tools_agent::AskQuestionTool::no_user()));

    let factory: ToolExecutorFactory = Box::new(move |conv_store, call_id_to_thread| {
        let mut registry = ToolRegistry::new();
        registry.register_arc(android_tool);
        registry.register_arc(ask_tool);
        Box::new(ToolExecutor::with_shared_store(
            Arc::new(registry),
            HashSet::new(),
            call_id_to_thread,
            conv_store,
        ))
    });

    let mut builder = RuntimeBuilder::new(config, "ui-test")
        .with_allow_interactive_oauth(false)
        .with_tool_executor_override(factory);
    if let Some(provider) = overrides.model_provider {
        builder = builder.with_model_provider(provider);
    }

    let bundle = builder
        .build_session()
        .await
        .map_err(|e| format!("could not build the ui-test kernel session: {e:#}"))?;

    let sink = bundle.handle.sink();
    // Answering the kernel's gates is mandatory: a turn whose gate is never
    // answered waits forever. Nothing outside this process is listening
    // here, so they are answered at once exactly as every other headless
    // surface answers them - unless the dispatching host brought a responder
    // (`on_human_gate`).
    match overrides.on_human_gate.clone() {
        Some(responder) => tokio::spawn(bundle.channels.forward_to(responder)),
        None => tokio::spawn(bundle.channels.answer_unattended()),
    };

    let mut status_rx = bundle.runtime.status_watch();

    // Progress relay. Spawned before the step is posted so the very first
    // state the machine enters is reported, and it exits on its own when the
    // machine reaches a terminal state -- never left running past the step it
    // describes.
    if let Some(on_state) = overrides.on_state.clone() {
        let mut progress_rx = bundle.runtime.status_watch();
        tokio::spawn(async move {
            let mut last = String::new();
            loop {
                let (label, done) = {
                    let status = progress_rx.borrow_and_update();
                    (status.state_label.clone(), status.done)
                };
                if label != last {
                    on_state(&label);
                    last = label;
                }
                if done || progress_rx.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    if !sink.emit(Event::UserMessage { text: script }).await {
        return Err(
            "ui-test kernel event queue closed before the step could be posted".to_string(),
        );
    }

    // No externally-imposed timeout (the dispatching host enforces none) -
    // bounded instead by `UiTestMachine`'s own per-step retry budget, which
    // always drives the machine to a terminal `Done`/`Failed` state.
    if status_rx.wait_for(|s| s.done).await.is_err() {
        return Err(
            "ui-test kernel runtime shut down before the step reached a terminal state".to_string(),
        );
    }

    let report = bundle
        .runtime
        .join()
        .await
        .map_err(|e| format!("ui-test kernel task did not join cleanly: {e}"))?;
    let ctx = report.ctx;

    if let Some(error) = ctx.fact(ERROR_FACT).and_then(Value::as_str) {
        return Err(error.to_string());
    }

    let step = ctx
        .fact(RESULTS_FACT)
        .and_then(|v| v.as_array().cloned())
        .and_then(|results| results.into_iter().next())
        .unwrap_or(Value::Null);

    let mut output = json!({ "passed": true, "step": step });
    if let Some((name, value)) = ask_user_binding(&ctx) {
        output[name] = Value::String(value);
    }
    Ok(output)
}

/// Resolves which real ADB serial this step's `AndroidTool` should default
/// to, given the host's own `device` info and `lister`'s current view of
/// what's attached.
///
/// `device` is `None` (no device requirement on this node at all) preserves
/// the pre-existing behaviour exactly: fall back to `SVEN_ANDROID_SERIAL`,
/// then let `AndroidTool`'s own per-call auto-detect handle it - there is no
/// host-supplied identity to validate against reality here at all.
///
/// `device` is `Some` routes through `adb::resolve_serial_validated`
/// (`sven-tools-android`'s own real device-selection/auto-detect - reused,
/// not reimplemented): an exact match on `device.serial` wins outright;
/// otherwise, when exactly one real device is attached, it is used
/// automatically with a logged warning naming the substitution (this is the
/// actual "auto-detect" behaviour a user hitting a stale/mismatched
/// `device_id`-as-serial bug asked for); only genuinely ambiguous cases
/// (zero attached, or two-or-more with no exact match) hard-fail.
///
/// # Errors
///
/// A descriptive error naming `device.device_id` (the host's own catalog
/// identity, for a human reading the failure) when resolution is genuinely
/// ambiguous, or when listing devices itself fails (e.g. `adb` not on
/// `PATH`).
async fn resolve_effective_serial(
    device: Option<&UiTestDevice>,
    lister: &dyn DeviceLister,
) -> Result<Option<String>, String> {
    let Some(d) = device else {
        return Ok(std::env::var("SVEN_ANDROID_SERIAL").ok());
    };
    let requested = d.serial.as_deref();
    match adb::resolve_serial_validated(lister, requested)
        .await
        .map_err(|e| format!("resolving device '{}': {e}", d.device_id))?
    {
        SerialPick::Resolved(s) => Ok(Some(s)),
        SerialPick::FellBackToSole(s) => {
            tracing::warn!(
                requested_device_id = %d.device_id,
                requested_serial = requested.unwrap_or("none"),
                fallback_serial = %s,
                "requested device '{}' (serial: {}) not found; falling back to the only attached \
                 device '{}'",
                d.device_id,
                requested.unwrap_or("none"),
                s
            );
            Ok(Some(s))
        }
        SerialPick::NoneAttached => Err(format!(
            "requested device '{}' (serial: {}) not found, and no ADB device is attached at all",
            d.device_id,
            requested.unwrap_or("none")
        )),
        SerialPick::Ambiguous(serials) => Err(format!(
            "requested device '{}' (serial: {}) not found, and {} ADB devices are attached with \
             no exact match ({}); fix the configured serial to disambiguate",
            d.device_id,
            requested.unwrap_or("none"),
            serials.len(),
            serials.join(", ")
        )),
    }
}

/// Every top-level `params` field except `instruction`, coerced to a string
/// for `UiTestScript::vars`. Non-string JSON values (numbers, bools,
/// objects, arrays) are serialized verbatim rather than dropped - a
/// resolved upstream value can be any JSON type (per the dispatch
/// contract), but `UiTestMachine`'s variable slots are always text
/// (they only ever feed `type_text`/`value_ref`).
fn extract_vars(params: &Value) -> BTreeMap<String, String> {
    let Some(obj) = params.as_object() else {
        return BTreeMap::new();
    };
    obj.iter()
        .filter(|(k, _)| k.as_str() != "instruction")
        .map(|(k, v)| {
            let s = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            (k.clone(), s)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use async_trait::async_trait;
    use sven_model_mock::ScriptedMockProvider;
    use sven_tool_api::policy::ApprovalPolicy;
    use sven_tool_api::{ToolCall, ToolOutput};

    fn test_config() -> Config {
        let mut cfg = Config::default();
        cfg.model.provider = "mock".into();
        cfg.model.name = "mock-model".into();
        cfg
    }

    /// A fake `android`/`ask_question` tool standing in for a device that
    /// answers, recording every call it received so a test can assert on
    /// what the machine actually sent it.
    ///
    /// The query actions the machine now brackets every step with
    /// (`screen_is_secure`, `find_element`, `ui_signature`) get real
    /// well-formed answers; anything else gets the fixed `reply` the test
    /// configured. `ui_signature` returns a fresh digest on each call, so a
    /// step reads as having changed the screen - a test that wants the
    /// opposite says so explicitly.
    struct FakeTool {
        name: &'static str,
        capability: sven_hsm::ToolCapability,
        reply: String,
        calls: std::sync::Mutex<Vec<Value>>,
    }

    impl FakeTool {
        fn new(
            name: &'static str,
            capability: sven_hsm::ToolCapability,
            reply: impl Into<String>,
        ) -> Arc<Self> {
            Arc::new(Self {
                name,
                capability,
                reply: reply.into(),
                calls: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "fake tool for ui-test dispatch tests"
        }
        fn parameters_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn default_policy(&self) -> ApprovalPolicy {
            ApprovalPolicy::Auto
        }
        fn kernel_capability(&self) -> sven_hsm::ToolCapability {
            self.capability
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            self.calls.lock().unwrap().push(call.args.clone());
            let nth = self.calls.lock().unwrap().len();
            let reply = match call.args.get("action").and_then(Value::as_str) {
                Some("screen_is_secure") => json!({ "secure_screen": false }).to_string(),
                Some("ui_signature") => json!({ "signature": format!("sig-{nth}") }).to_string(),
                Some("find_element") => json!({
                    "found": true, "x": 10, "y": 20, "via": "exact",
                    "signature": format!("sig-{nth}")
                })
                .to_string(),
                _ => self.reply.clone(),
            };
            ToolOutput::ok(&call.id, reply)
        }
    }

    /// The first recorded call performing `action`.
    ///
    /// Every device-acting step is now bracketed by query calls
    /// (`screen_is_secure` before, `ui_signature` before and after), so a
    /// test that indexes a fixed position asserts on the bracket instead of
    /// on the action it means.
    fn call_performing(tool: &FakeTool, action: &str) -> Value {
        tool.calls
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.get("action").and_then(Value::as_str) == Some(action))
            .unwrap_or_else(|| panic!("no '{action}' call was made"))
            .clone()
    }

    fn compiled_step_reply(compiled: Value) -> ScriptedMockProvider {
        ScriptedMockProvider::always_text(compiled.to_string())
    }

    /// Like `compiled_step_reply`, but scripted for every attempt a step
    /// exhausting its full retry budget makes (`ScriptedMockProvider` does
    /// not repeat a script once consumed - see its own `complete` doc).
    /// `attempts` mirrors `UiTestMachine`'s own `MAX_ATTEMPTS_PER_STEP`.
    fn compiled_step_reply_for_every_retry(
        compiled: Value,
        attempts: usize,
    ) -> ScriptedMockProvider {
        let text = compiled.to_string();
        ScriptedMockProvider::new(
            (0..attempts)
                .map(|_| {
                    vec![
                        sven_model::ResponseEvent::TextDelta(text.clone()),
                        sven_model::ResponseEvent::Done,
                    ]
                })
                .collect(),
        )
    }

    #[tokio::test]
    async fn a_launch_app_step_succeeds_and_reports_the_step_outcome() {
        let android = FakeTool::new(
            "android",
            sven_hsm::ToolCapability::ControlDevice,
            "launched com.example.demoapp",
        );
        let overrides = UiTestDispatchOverrides {
            model_provider: Some(Box::new(compiled_step_reply(
                json!({ "verb": "launch_app", "target": "com.example.demoapp" }),
            ))),
            android_tool: Some(android.clone() as Arc<dyn Tool>),
            ..Default::default()
        };

        let out = dispatch_ui_test_step(
            Arc::new(test_config()),
            None,
            &json!({ "instruction": "Launch the demo app" }),
            overrides,
        )
        .await
        .expect("a well-formed launch_app step must succeed");

        assert_eq!(out["passed"], true);
        assert_eq!(out["step"]["passed"], true);
        assert_eq!(out["step"]["instruction"], "Launch the demo app");
        let launched = call_performing(&android, "launch_app");
        assert_eq!(launched["package"], "com.example.demoapp");
    }

    /// A fake [`DeviceLister`] returning a fixed, canned device list - so a
    /// test can exercise the exact/fallback/ambiguous paths through
    /// `resolve_effective_serial` without a real `adb devices` invocation.
    struct FakeLister(Vec<adb::DeviceEntry>);
    #[async_trait]
    impl DeviceLister for FakeLister {
        async fn list_devices(&self) -> Result<Vec<adb::DeviceEntry>, String> {
            Ok(self.0.clone())
        }
    }
    fn ready(serial: &str) -> adb::DeviceEntry {
        adb::DeviceEntry {
            serial: serial.to_string(),
            state: "device".to_string(),
        }
    }

    #[tokio::test]
    async fn a_device_field_does_not_prevent_the_step_from_running() {
        // `dispatch_ui_test_step` resolves `device.serial` (the real ADB
        // identity, distinct from `device.device_id`, the host's own catalog
        // key) into `AndroidTool::new`'s default serial when using the real
        // tool - see `resolve_effective_serial`'s own doc. With a fake tool
        // substituted, this only proves the `device` field is accepted and
        // plumbed through without breaking the run.
        let android = FakeTool::new("android", sven_hsm::ToolCapability::ControlDevice, "ok");
        let overrides = UiTestDispatchOverrides {
            model_provider: Some(Box::new(compiled_step_reply(
                json!({ "verb": "key_event", "target": "HOME" }),
            ))),
            android_tool: Some(android as Arc<dyn Tool>),
            device_lister: Some(Arc::new(FakeLister(vec![ready("ec677a50")]))),
            ..Default::default()
        };
        let device = UiTestDevice {
            provider_id: "local".into(),
            device_id: "phone-1".into(),
            serial: Some("ec677a50".into()),
            ..Default::default()
        };

        let out = dispatch_ui_test_step(
            Arc::new(test_config()),
            Some(&device),
            &json!({ "instruction": "Go home" }),
            overrides,
        )
        .await
        .expect("must succeed");
        assert_eq!(out["passed"], true);
    }

    // ─── resolve_effective_serial: the device-fallback logic itself ───────
    // This is the "auto-detect instead of hard-failing on the host's own
    // catalog id" behaviour the android-ui-test/device-identity fix exists
    // for. Every case is a unit test against a `FakeLister`, per this
    // repo's TDD convention for `sven-tools-android`'s own device-selection
    // tests (no real `adb`, no real device needed).

    #[tokio::test]
    async fn no_device_at_all_falls_back_to_the_env_var_without_touching_the_lister() {
        struct PanicsIfCalled;
        #[async_trait]
        impl DeviceLister for PanicsIfCalled {
            async fn list_devices(&self) -> Result<Vec<adb::DeviceEntry>, String> {
                panic!("must not be called when no device info was supplied at all");
            }
        }
        let resolved = resolve_effective_serial(None, &PanicsIfCalled)
            .await
            .expect("must succeed");
        // SVEN_ANDROID_SERIAL is not set in this test process by default.
        assert_eq!(resolved, std::env::var("SVEN_ANDROID_SERIAL").ok());
    }

    #[tokio::test]
    async fn an_exact_serial_match_is_used_directly() {
        let device = UiTestDevice {
            provider_id: "local".into(),
            device_id: "phone-1".into(),
            serial: Some("ec677a50".into()),
            ..Default::default()
        };
        let lister = FakeLister(vec![ready("ec677a50")]);
        let resolved = resolve_effective_serial(Some(&device), &lister)
            .await
            .expect("must succeed");
        assert_eq!(resolved, Some("ec677a50".to_string()));
    }

    #[tokio::test]
    async fn a_mismatched_serial_falls_back_to_the_only_attached_device() {
        // The exact bug: the host sent its catalog id, not a real serial, and
        // it happens not to match anything attached - but exactly one real
        // device is, so this must succeed via fallback, not hard-fail.
        let device = UiTestDevice {
            provider_id: "local".into(),
            device_id: "phone-1".into(),
            serial: Some("phone-1".into()),
            ..Default::default()
        };
        let lister = FakeLister(vec![ready("ec677a50")]);
        let resolved = resolve_effective_serial(Some(&device), &lister)
            .await
            .expect("an unambiguous single attached device must succeed, not hard-fail");
        assert_eq!(resolved, Some("ec677a50".to_string()));
    }

    #[tokio::test]
    async fn a_missing_serial_falls_back_to_the_only_attached_device_too() {
        // `serial: None` is the documented common case (devices.json need
        // not name a real serial at all) - it must degrade to the same
        // single-device auto-detect, not an error.
        let device = UiTestDevice {
            provider_id: "local".into(),
            device_id: "phone-1".into(),
            serial: None,
            ..Default::default()
        };
        let lister = FakeLister(vec![ready("ec677a50")]);
        let resolved = resolve_effective_serial(Some(&device), &lister)
            .await
            .expect("must succeed");
        assert_eq!(resolved, Some("ec677a50".to_string()));
    }

    #[tokio::test]
    async fn zero_attached_devices_is_a_hard_failure_naming_the_requested_device() {
        let device = UiTestDevice {
            provider_id: "local".into(),
            device_id: "phone-1".into(),
            serial: Some("ec677a50".into()),
            ..Default::default()
        };
        let lister = FakeLister(vec![]);
        let err = resolve_effective_serial(Some(&device), &lister)
            .await
            .expect_err("zero attached devices is genuinely ambiguous - it must fail");
        assert!(err.contains("phone-1"), "{err}");
    }

    #[tokio::test]
    async fn two_attached_devices_with_no_exact_match_is_a_hard_failure() {
        let device = UiTestDevice {
            provider_id: "local".into(),
            device_id: "phone-1".into(),
            serial: Some("phone-1".into()),
            ..Default::default()
        };
        let lister = FakeLister(vec![ready("aaa"), ready("bbb")]);
        let err = resolve_effective_serial(Some(&device), &lister)
            .await
            .expect_err("two candidates with no exact match is genuinely ambiguous");
        assert!(err.contains("phone-1"), "{err}");
        assert!(err.contains("aaa") && err.contains("bbb"), "{err}");
    }

    #[tokio::test]
    async fn two_attached_devices_with_an_exact_match_still_resolves() {
        // Ambiguity is about the DECISION, not the device count - an exact
        // match is never ambiguous even with other devices also attached.
        let device = UiTestDevice {
            provider_id: "local".into(),
            device_id: "phone-1".into(),
            serial: Some("bbb".into()),
            ..Default::default()
        };
        let lister = FakeLister(vec![ready("aaa"), ready("bbb")]);
        let resolved = resolve_effective_serial(Some(&device), &lister)
            .await
            .expect("an exact match must resolve even with other devices attached");
        assert_eq!(resolved, Some("bbb".to_string()));
    }

    #[tokio::test]
    async fn other_params_fields_are_seeded_as_vars_and_resolve_through_value_ref() {
        let android = FakeTool::new("android", sven_hsm::ToolCapability::ControlDevice, "typed");
        let overrides = UiTestDispatchOverrides {
            model_provider: Some(Box::new(compiled_step_reply(
                json!({ "verb": "type_text", "value_ref": "code" }),
            ))),
            android_tool: Some(android.clone() as Arc<dyn Tool>),
            ..Default::default()
        };

        let out = dispatch_ui_test_step(
            Arc::new(test_config()),
            None,
            &json!({ "instruction": "Enter the code", "code": "123456" }),
            overrides,
        )
        .await
        .expect("must succeed");

        assert_eq!(out["passed"], true);
        assert_eq!(call_performing(&android, "type_text")["text"], "123456");
    }

    /// A dispatched step is otherwise a black box for however long it takes
    /// -- minutes, for a real UI step. `on_state` is what lets a host say
    /// what it is doing meanwhile.
    #[tokio::test]
    async fn on_state_reports_each_distinct_state_the_step_passes_through() {
        let android = FakeTool::new("android", sven_hsm::ToolCapability::ControlDevice, "tapped");
        let seen: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let overrides = UiTestDispatchOverrides {
            model_provider: Some(Box::new(compiled_step_reply(
                json!({ "verb": "tap", "target": "OK" }),
            ))),
            android_tool: Some(android.clone() as Arc<dyn Tool>),
            on_state: Some(Arc::new(move |label: &str| {
                recorder
                    .lock()
                    .expect("recorder mutex")
                    .push(label.to_string());
            })),
            ..Default::default()
        };

        let out = dispatch_ui_test_step(
            Arc::new(test_config()),
            None,
            &json!({ "instruction": "Tap OK" }),
            overrides,
        )
        .await
        .expect("must succeed");
        assert_eq!(out["passed"], true);

        let labels = seen.lock().expect("recorder mutex").clone();
        assert!(
            !labels.is_empty(),
            "a step that ran must have reported at least the state it started in"
        );
        // DISTINCT states, not every status update: the watch channel
        // republishes on every processed event, and relaying that verbatim
        // would send a user hundreds of identical messages.
        assert!(
            labels.windows(2).all(|pair| pair[0] != pair[1]),
            "the same state must never be reported twice in a row: {labels:?}"
        );
    }

    /// Non-vacuity for the spawn: a caller that asks for nothing pays for
    /// nothing, and the step behaves identically.
    #[tokio::test]
    async fn no_on_state_means_no_relay_and_an_unchanged_result() {
        let android = FakeTool::new("android", sven_hsm::ToolCapability::ControlDevice, "tapped");
        let overrides = UiTestDispatchOverrides {
            model_provider: Some(Box::new(compiled_step_reply(
                json!({ "verb": "tap", "target": "OK" }),
            ))),
            android_tool: Some(android as Arc<dyn Tool>),
            ..Default::default()
        };
        let out = dispatch_ui_test_step(
            Arc::new(test_config()),
            None,
            &json!({ "instruction": "Tap OK" }),
            overrides,
        )
        .await
        .expect("must succeed");
        assert_eq!(out["passed"], true);
    }

    #[tokio::test]
    async fn a_non_string_params_field_is_seeded_as_its_json_text() {
        let android = FakeTool::new("android", sven_hsm::ToolCapability::ControlDevice, "ok");
        let overrides = UiTestDispatchOverrides {
            model_provider: Some(Box::new(compiled_step_reply(
                json!({ "verb": "type_text", "value_ref": "retries" }),
            ))),
            android_tool: Some(android.clone() as Arc<dyn Tool>),
            ..Default::default()
        };

        let out = dispatch_ui_test_step(
            Arc::new(test_config()),
            None,
            &json!({ "instruction": "Type retries", "retries": 3 }),
            overrides,
        )
        .await
        .expect("must succeed");

        assert_eq!(out["passed"], true);
        assert_eq!(call_performing(&android, "type_text")["text"], "3");
    }

    #[tokio::test]
    async fn an_ask_user_steps_answer_is_a_named_field_in_the_output_not_buried() {
        let ask = FakeTool::new("ask_question", sven_hsm::ToolCapability::ReadFile, "1234");
        let overrides = UiTestDispatchOverrides {
            model_provider: Some(Box::new(compiled_step_reply(
                json!({ "verb": "ask_user", "target": "What is the code?", "bind": "code" }),
            ))),
            ask_tool: Some(ask as Arc<dyn Tool>),
            ..Default::default()
        };

        let out = dispatch_ui_test_step(
            Arc::new(test_config()),
            None,
            &json!({ "instruction": "Ask the user for the code" }),
            overrides,
        )
        .await
        .expect("must succeed");

        assert_eq!(out["passed"], true);
        assert_eq!(
            out["code"], "1234",
            "the ask_user answer must be a clearly-named top-level field, not buried in step"
        );
    }

    #[tokio::test]
    async fn a_failed_step_that_exhausts_its_retry_budget_is_a_clear_error_not_a_panic() {
        struct AlwaysFailAndroid;
        #[async_trait]
        impl Tool for AlwaysFailAndroid {
            fn name(&self) -> &str {
                "android"
            }
            fn description(&self) -> &str {
                "always fails"
            }
            fn parameters_schema(&self) -> Value {
                json!({ "type": "object" })
            }
            fn default_policy(&self) -> ApprovalPolicy {
                ApprovalPolicy::Auto
            }
            fn kernel_capability(&self) -> sven_hsm::ToolCapability {
                sven_hsm::ToolCapability::ControlDevice
            }
            async fn execute(&self, call: &ToolCall) -> ToolOutput {
                ToolOutput::err(&call.id, "device not found")
            }
        }

        let overrides = UiTestDispatchOverrides {
            model_provider: Some(Box::new(compiled_step_reply_for_every_retry(
                json!({ "verb": "launch_app", "target": "com.example.demoapp" }),
                3,
            ))),
            android_tool: Some(Arc::new(AlwaysFailAndroid)),
            ..Default::default()
        };

        let err = dispatch_ui_test_step(
            Arc::new(test_config()),
            None,
            &json!({ "instruction": "Launch the demo app" }),
            overrides,
        )
        .await
        .expect_err("an exhausted retry budget must be a clear Err, not a fabricated success");

        assert!(err.contains("device not found"), "{err}");
    }

    #[tokio::test]
    async fn a_missing_instruction_field_is_a_clear_error() {
        let err = dispatch_ui_test_step(
            Arc::new(test_config()),
            None,
            &json!({}),
            UiTestDispatchOverrides::default(),
        )
        .await
        .expect_err("params without an instruction must be refused");
        assert!(err.contains("instruction"), "{err}");
    }
}
