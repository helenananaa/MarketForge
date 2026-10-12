//! Trusted local process plugins. Commands are installed by the operator,
//! never supplied by a room or an HTTP caller. No shell is involved.
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crate::{
    bot_dispatch::{CURSOR, Trigger},
    bot_workers::{WorkerConfig, WorkerPool},
};
use exchange_core::{
    AgentTemplate, BOT_PROTOCOL_VERSION, BotConfig, BotDecisionRequest, BotDecisionResponse,
    BotDescriptor, BotError, BotFactory, BotRegistry, MAX_BOT_ACTIONS, OrderAction,
    ParticipantObservation, PersistedAgentKindState, ScheduledBot,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

pub const BOT_PLUGIN_DIR_ENV: &str = "MARKETFORGE_BOT_PLUGIN_DIR";
pub(super) const MAX_MESSAGE_BYTES: usize = 1_048_576;
pub(super) const MAX_REQUEST_BYTES: usize = 4 * MAX_MESSAGE_BYTES;
pub(super) const MAX_STDERR_BYTES: usize = 65_536;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessBotManifest {
    pub bot: BotDescriptor,
    pub command: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_data: Option<ProcessMarketData>,
    #[serde(default)]
    pub related_instruments_parameter: Option<String>,
    #[serde(default)]
    pub additional_intervals_parameter: Option<String>,
    #[serde(default)]
    pub account_events: bool,
    /// Experimental host prefilter; disabled until the workload is qualified.
    #[serde(default)]
    pub host_event_prefilter: bool,
    #[serde(default)]
    pub compatible_versions: Vec<String>,
    #[serde(default)]
    pub(super) decision_trigger: Trigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) worker_pool: Option<WorkerConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessMarketData {
    pub interval_parameter: String,
    pub max_bars: usize,
}
fn default_timeout() -> u64 {
    1_000
}

#[derive(Clone)]
struct ProcessBotFactory {
    manifest: ProcessBotManifest,
    directory: PathBuf,
    pool: Option<Arc<WorkerPool>>,
    event_prefilter_enabled: bool,
}

/// Read sorted <root>/<package>/bot.json manifests once at startup.
pub fn load_bot_plugins(root: &Path) -> Result<BotRegistry, BotError> {
    let mut registry = BotRegistry::with_builtins();
    let mut directories = fs::read_dir(root)
        .map_err(bot_io)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(bot_io)?;
    directories.sort_by_key(|entry| entry.file_name());
    for entry in directories {
        if !entry.file_type().map_err(bot_io)?.is_dir() {
            continue;
        }
        let directory = entry.path().canonicalize().map_err(bot_io)?;
        let path = directory.join("bot.json");
        if !path.is_file() {
            continue;
        }
        let bytes = fs::read(path).map_err(bot_io)?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(BotError("bot manifest exceeds size limit".into()));
        }
        let manifest: ProcessBotManifest = serde_json::from_slice(&bytes).map_err(bot_io)?;
        if manifest.bot.runtime != "process"
            || manifest.command.is_empty()
            || manifest.command[0].is_empty()
            || !(1..=10_000).contains(&manifest.timeout_ms)
        {
            return Err(BotError(format!(
                "invalid process bot manifest: {}",
                manifest.bot.id
            )));
        }
        if let Some(data) = &manifest.market_data {
            let parameter = manifest
                .bot
                .parameters
                .get(&data.interval_parameter)
                .ok_or_else(|| BotError("market data interval parameter is missing".into()))?;
            if !matches!(parameter.kind, exchange_core::bots::ParameterType::Integer)
                || parameter.minimum.is_none_or(|min| min < 1)
                || !(1..=4096).contains(&data.max_bars)
            {
                return Err(BotError("invalid process market data declaration".into()));
            }
        }
        for name in [
            &manifest.related_instruments_parameter,
            &manifest.additional_intervals_parameter,
        ]
        .into_iter()
        .flatten()
        {
            if manifest
                .bot
                .parameters
                .get(name)
                .is_none_or(|p| !matches!(p.kind, exchange_core::bots::ParameterType::Array))
            {
                return Err(BotError(
                    "process data parameter must name a declared array".into(),
                ));
            }
        }
        if (manifest.account_events || manifest.additional_intervals_parameter.is_some())
            && manifest.market_data.is_none()
        {
            return Err(BotError(
                "account events and intervals require market_data".into(),
            ));
        }
        manifest
            .decision_trigger
            .validate(manifest.market_data.is_some())?;
        if let Some(config) = &manifest.worker_pool {
            config.validate()?;
        }
        let pool = manifest.worker_pool.clone().map(WorkerPool::new);
        let event_prefilter_enabled = manifest.host_event_prefilter
            || std::env::var("MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER").as_deref() == Ok("1");
        registry.register(ProcessBotFactory {
            manifest,
            directory,
            pool,
            event_prefilter_enabled,
        })?;
    }
    Ok(registry)
}

pub fn bot_registry_from_env() -> Result<BotRegistry, BotError> {
    match std::env::var_os(BOT_PLUGIN_DIR_ENV) {
        Some(root) => load_bot_plugins(Path::new(&root)),
        None => Ok(BotRegistry::with_builtins()),
    }
}

impl BotFactory for ProcessBotFactory {
    fn supports_instance_reuse(&self) -> bool {
        // Retaining the exact expected state has a memory cost. Only opt-in
        // eligible event hosts reuse this adapter, after the scheduler's guards.
        self.event_filter().is_some()
    }
    fn event_filter(&self) -> Option<exchange_core::bots::BotEventFilter> {
        if !self.event_prefilter_enabled {
            return None;
        }
        match self.manifest.decision_trigger {
            crate::bot_dispatch::Trigger::Events {
                closed_bars,
                book: false,
                account,
                interval_ms,
            } => Some(exchange_core::bots::BotEventFilter {
                closed_bars,
                account,
                interval_ms,
            }),
            _ => None,
        }
    }
    fn decision_is_due(
        &self,
        template: &AgentTemplate,
        state: &PersistedAgentKindState,
        observation: &ParticipantObservation,
    ) -> Result<bool, BotError> {
        let AgentTemplate::Plugin(config) = template else {
            return Err(BotError("process bot requires Plugin config".into()));
        };
        let PersistedAgentKindState::Plugin { data, .. } = state else {
            return Ok(true);
        };
        let mut config = config.clone();
        config.config = self.manifest.bot.validate_config(&config.config)?;
        self.manifest
            .decision_trigger
            .due(&config, data, observation)
    }
    fn supports_plugin_version(&self, version: &str) -> bool {
        self.manifest.bot.version == version
            || self
                .manifest
                .compatible_versions
                .iter()
                .any(|v| v == version)
    }
    fn account_events(&self) -> bool {
        self.manifest.account_events
    }
    fn additional_intervals(&self, template: &AgentTemplate) -> Result<Vec<u64>, BotError> {
        let Some(name) = &self.manifest.additional_intervals_parameter else {
            return Ok(vec![]);
        };
        let AgentTemplate::Plugin(config) = template else {
            return Err(BotError("process bot requires Plugin config".into()));
        };
        let config = self.manifest.bot.validate_config(&config.config)?;
        let values = config[name]
            .as_array()
            .ok_or_else(|| BotError("additional intervals must be an array".into()))?;
        values
            .iter()
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| BotError("interval must be a positive integer".into()))
            })
            .collect()
    }
    fn descriptor(&self) -> &BotDescriptor {
        &self.manifest.bot
    }
    fn related_instruments(&self, template: &AgentTemplate) -> Result<Vec<String>, BotError> {
        let Some(name) = &self.manifest.related_instruments_parameter else {
            return Ok(vec![]);
        };
        let AgentTemplate::Plugin(config) = template else {
            return Err(BotError("process bot requires Plugin config".into()));
        };
        let config = self.manifest.bot.validate_config(&config.config)?;
        let values = config[name]
            .as_array()
            .ok_or_else(|| BotError("related instruments must be an array".into()))?;
        values
            .iter()
            .map(|v| {
                v.as_str()
                    .map(String::from)
                    .ok_or_else(|| BotError("related instrument must be a string".into()))
            })
            .collect()
    }
    fn market_data_request(
        &self,
        template: &AgentTemplate,
    ) -> Result<Option<exchange_core::bots::BotMarketDataRequest>, BotError> {
        let Some(data) = &self.manifest.market_data else {
            return Ok(None);
        };
        let AgentTemplate::Plugin(config) = template else {
            return Err(BotError("process bot requires Plugin config".into()));
        };
        let config = self.manifest.bot.validate_config(&config.config)?;
        let interval_ms = config[&data.interval_parameter]
            .as_u64()
            .ok_or_else(|| BotError("market data interval must be a positive integer".into()))?;
        Ok(Some(exchange_core::bots::BotMarketDataRequest {
            interval_ms,
            max_bars: data.max_bars,
        }))
    }
    fn create(
        &self,
        template: &AgentTemplate,
        state: &PersistedAgentKindState,
    ) -> Result<Box<dyn ScheduledBot>, BotError> {
        let AgentTemplate::Plugin(config) = template else {
            return Err(BotError("process bot requires Plugin config".into()));
        };
        let PersistedAgentKindState::Plugin { data, .. } = state else {
            return Err(BotError("process bot requires Plugin state".into()));
        };
        let mut config = config.clone();
        config.config = self.manifest.bot.validate_config(&config.config)?;
        Ok(Box::new(ProcessBot {
            factory: self.clone(),
            config,
            state: data.clone(),
        }))
    }
}

struct ProcessBot {
    factory: ProcessBotFactory,
    config: BotConfig,
    state: Arc<Value>,
}
impl ScheduledBot for ProcessBot {
    fn decide(
        &mut self,
        observation: &ParticipantObservation,
    ) -> Result<Vec<OrderAction>, BotError> {
        if !self
            .factory
            .manifest
            .decision_trigger
            .due(&self.config, &self.state, observation)?
        {
            return Ok(vec![]);
        }
        let cursor = self.factory.manifest.decision_trigger.fingerprint(
            &self.config,
            &self.state,
            observation,
        )?;
        #[cfg(feature = "bench-execution-timing")]
        let request_clone_timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_REQUEST_CLONE,
        );
        let request = BotDecisionRequest {
            protocol_version: BOT_PROTOCOL_VERSION.into(),
            plugin_id: self.config.plugin_id.clone(),
            plugin_version: self.config.plugin_version.clone(),
            state_version: self.config.state_version,
            participant: self.config.participant.clone(),
            seed: self.config.seed,
            config: self.config.config.clone(),
            state: self.state.as_ref().clone(),
            observation: observation.clone(),
        };
        #[cfg(feature = "bench-execution-timing")]
        drop(request_clone_timer);
        let response = self.factory.exchange(&request)?;
        if response.protocol_version != BOT_PROTOCOL_VERSION
            || response.plugin_id != self.config.plugin_id
            || response.plugin_version != self.config.plugin_version
            || response.state_version != self.config.state_version
        {
            return Err(BotError(format!(
                "bot {} response version mismatch",
                self.config.plugin_id
            )));
        }
        if response.actions.len() > MAX_BOT_ACTIONS {
            return Err(BotError("bot exceeded action limit".into()));
        }
        let mut next = response.state;
        if let Some(cursor) = cursor {
            let state = next
                .as_object_mut()
                .ok_or_else(|| BotError("event-driven worker state must be an object".into()))?;
            state.insert(CURSOR.into(), Value::String(cursor));
            state.insert(
                "_host_observation".into(),
                serde_json::json!([observation.step, observation.market_time_ms]),
            );
        }
        self.state = next.into();
        Ok(response.actions)
    }
    fn snapshot(&self) -> PersistedAgentKindState {
        PersistedAgentKindState::Plugin {
            plugin_id: self.config.plugin_id.clone(),
            plugin_version: self.config.plugin_version.clone(),
            state_version: self.config.state_version,
            data: self.state.clone(),
        }
    }
    fn prepare_reused_call(&mut self, state: &PersistedAgentKindState) -> Result<(), BotError> {
        #[cfg(feature = "bench-execution-timing")]
        let _timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_STATE_RESTORE,
        );
        match state {
            PersistedAgentKindState::Plugin {
                plugin_id,
                plugin_version,
                state_version,
                data,
            } if plugin_id == &self.config.plugin_id
                && plugin_version == &self.config.plugin_version
                && *state_version == self.config.state_version =>
            {
                self.state = data.clone();
                Ok(())
            }
            _ => Err(BotError("cached plugin state identity mismatch".into())),
        }
    }
    fn take_snapshot(&mut self) -> PersistedAgentKindState {
        PersistedAgentKindState::Plugin {
            plugin_id: self.config.plugin_id.clone(),
            plugin_version: self.config.plugin_version.clone(),
            state_version: self.config.state_version,
            data: std::mem::take(&mut self.state),
        }
    }
    fn execution_feedback(
        &mut self,
        action: &OrderAction,
        execution: &exchange_core::GatewayExecution,
    ) {
        let mut state = self.snapshot();
        exchange_core::bots::record_bot_execution(&mut state, action, execution);
        if let PersistedAgentKindState::Plugin { data, .. } = state {
            self.state = data;
        }
    }
}

pub(super) fn bot_io(error: impl std::fmt::Display) -> BotError {
    BotError(error.to_string())
}

/// Kill/reap even when an IO error or malformed message ends the exchange.
pub(super) struct ManagedChild(pub(super) Child);
impl Drop for ManagedChild {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // Each plugin owns a new process group. Kill descendants holding pipes too.
            // SAFETY: the negative PID addresses only the group we created at spawn.
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn process_command(directory: &Path, args: &[String]) -> Command {
    let mut command = Command::new(&args[0]);
    command
        .args(&args[1..])
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    // Interpreter lookup and Windows process startup; backend credentials are not inherited.
    for key in ["PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(feature = "bench-execution-timing")]
    if let Some(value) = std::env::var_os("PINE_BENCH_PY_TRACE_DIR") {
        command.env("PINE_BENCH_PY_TRACE_DIR", value);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

impl ProcessBotFactory {
    fn exchange(&self, request: &BotDecisionRequest) -> Result<BotDecisionResponse, BotError> {
        if let Some(pool) = &self.pool {
            return pool.exchange(&self.directory, &self.manifest, request);
        }
        let mut input = serde_json::to_vec(request).map_err(bot_io)?;
        input.push(b'\n');
        if input.len() > MAX_REQUEST_BYTES {
            return Err(BotError("bot request exceeds size limit".into()));
        }
        let mut command = process_command(&self.directory, &self.manifest.command);
        let mut child = ManagedChild(command.spawn().map_err(bot_io)?);
        let mut stdin = child.0.stdin.take().unwrap();
        let stdout = child.0.stdout.take().unwrap();
        let stderr = child.0.stderr.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let writer_sender = sender.clone();
        let writer = thread::spawn(move || {
            let _ = writer_sender.send((0, stdin.write_all(&input).map(|_| Vec::new())));
        });
        let stdout_sender = sender.clone();
        let reader = thread::spawn(move || {
            let _ = stdout_sender.send((1, bounded_read(stdout, MAX_MESSAGE_BYTES)));
        });
        let error_reader = thread::spawn(move || {
            let _ = sender.send((2, bounded_read(stderr, MAX_STDERR_BYTES)));
        });
        let deadline = Instant::now() + Duration::from_millis(self.manifest.timeout_ms);
        let result = (|| {
            let mut output = None;
            let mut diagnostics = Vec::new();
            for _ in 0..3 {
                let remaining = deadline.saturating_duration_since(Instant::now());
                let (stream, bytes) = receiver
                    .recv_timeout(remaining)
                    .map_err(|_| BotError(format!("bot {} timed out", self.manifest.bot.id)))?;
                let bytes = bytes.map_err(bot_io)?;
                match stream {
                    1 => output = Some(bytes),
                    2 => diagnostics = bytes,
                    _ => {}
                }
            }
            let status = loop {
                if let Some(status) = child.0.try_wait().map_err(bot_io)? {
                    break status;
                }
                if Instant::now() >= deadline {
                    return Err(BotError(format!("bot {} timed out", self.manifest.bot.id)));
                }
                thread::sleep(Duration::from_millis(2));
            };
            if !status.success() {
                return Err(BotError(format!(
                    "bot {} exited {status}: {}",
                    self.manifest.bot.id,
                    String::from_utf8_lossy(&diagnostics)
                )));
            }
            // Exactly one JSON response. JSON whitespace, including its trailing newline, is allowed.
            serde_json::from_slice(&output.unwrap_or_default()).map_err(|error| {
                BotError(format!(
                    "bot {} invalid response: {error}",
                    self.manifest.bot.id
                ))
            })
        })();
        drop(child);
        #[cfg(unix)]
        {
            let _ = writer.join();
            let _ = reader.join();
            let _ = error_reader.join();
        }
        #[cfg(not(unix))]
        {
            drop((writer, reader, error_reader));
        }
        result
    }
}

fn bounded_read(reader: impl Read, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::other("bot output exceeds size limit"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange_core::{
        AccountSnapshot, CrashPoint, ParticipantConfig, ParticipantKind, RoomManager,
        ScenarioConfig, SchedulerMode, SchedulerState, run_scheduler_step_with_registry,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    fn example_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../bot-plugins")
    }
    fn scenario() -> ScenarioConfig {
        let value: Value =
            serde_json::from_str(include_str!("../../scripts/fixtures/f6_batch_spec.json"))
                .unwrap();
        serde_json::from_value(value["scenario"].clone()).unwrap()
    }
    fn template() -> AgentTemplate {
        AgentTemplate::Plugin(BotConfig {
            participant: ParticipantConfig {
                participant_id: "buyer".into(),
                kind: ParticipantKind::RuleAgent,
                room_id: "f6-batch".into(),
                account_id: 20,
                instrument_id: Some("V-BTC-SPOT".into()),
            },
            plugin_id: "example.buy-remaining".into(),
            plugin_version: "1.0.0".into(),
            config_version: 1,
            state_version: 1,
            seed: 7,
            config: serde_json::json!({"target_qty":2}),
        })
    }
    fn observation() -> ParticipantObservation {
        let mut rooms = RoomManager::new();
        rooms.create_room(scenario()).unwrap();
        rooms
            .participant_observation("f6-batch", "V-BTC-SPOT", 20)
            .unwrap()
    }

    #[test]
    fn pine_manifest_requests_only_bounded_configured_history_without_running_python() {
        let registry = load_bot_plugins(&example_root()).unwrap();
        let mut config = match template() {
            AgentTemplate::Plugin(value) => value,
            _ => unreachable!(),
        };
        config.plugin_id = "pine.strategy".into();
        config.config = serde_json::json!({"bar_interval_ms": 15000});
        let template = AgentTemplate::Plugin(config.clone());
        registry.validate_template(&template).unwrap();
        let request = registry.market_data_request(&template).unwrap().unwrap();
        assert_eq!(request.interval_ms, 15000);
        assert_eq!(request.max_bars, 2049);
        config.config = serde_json::json!({"bar_interval_ms": 0});
        assert!(
            registry
                .validate_template(&AgentTemplate::Plugin(config))
                .is_err()
        );
        assert!(
            registry
                .market_data_request(&super::tests::template())
                .unwrap()
                .is_none()
        );
    }
    struct Package(PathBuf);
    impl Package {
        fn new(program: &str, timeout_ms: u64) -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "marketforge-bots-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("test")).unwrap();
            let mut manifest: ProcessBotManifest =
                serde_json::from_str(include_str!("../../bot-plugins/buy-remaining/bot.json"))
                    .unwrap();
            manifest.timeout_ms = timeout_ms;
            fs::write(
                root.join("test/bot.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            fs::write(root.join("test/bot.py"), program).unwrap();
            Self(root)
        }
        fn registry(&self) -> BotRegistry {
            load_bot_plugins(&self.0).unwrap()
        }
    }
    impl Drop for Package {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn process_plugin_trades_and_saved_state_survives_restart_without_redecision() {
        for crash in [
            CrashPoint::AfterDecisionPersist {
                step: 1,
                participant_index: 0,
            },
            CrashPoint::BeforeActionSubmit {
                step: 1,
                participant_index: 0,
                action_index: 0,
            },
            CrashPoint::AfterActionSubmit {
                step: 1,
                participant_index: 0,
                action_index: 0,
            },
        ] {
            let registry = load_bot_plugins(&example_root()).unwrap();
            let mut rooms = RoomManager::new();
            rooms.create_room(scenario()).unwrap();
            let mut order_id = 3;
            let outcome = run_scheduler_step_with_registry(
                &mut rooms,
                &mut order_id,
                SchedulerState::new("f6-batch", vec![template()], SchedulerMode::Manual),
                crash,
                &registry,
            )
            .unwrap();
            assert!(outcome.crashed);
            let saved: SchedulerState =
                serde_json::from_slice(&serde_json::to_vec(&outcome.state).unwrap()).unwrap();
            let restarted_registry = load_bot_plugins(&example_root()).unwrap();
            let resumed = run_scheduler_step_with_registry(
                &mut rooms,
                &mut order_id,
                saved,
                CrashPoint::None,
                &restarted_registry,
            )
            .unwrap();
            let PersistedAgentKindState::Plugin { data, .. } = &resumed.state.agents[0].kind_state
            else {
                panic!()
            };
            assert_eq!(data["observed_steps"], 1);
            assert_eq!(rooms.execution_history("f6-batch").unwrap().len(), 3);
            let next = run_scheduler_step_with_registry(
                &mut rooms,
                &mut order_id,
                resumed.state,
                CrashPoint::None,
                &restarted_registry,
            )
            .unwrap();
            let last = run_scheduler_step_with_registry(
                &mut rooms,
                &mut order_id,
                next.state,
                CrashPoint::None,
                &restarted_registry,
            )
            .unwrap();
            let PersistedAgentKindState::Plugin { data, .. } = &last.state.agents[0].kind_state
            else {
                panic!()
            };
            assert_eq!(data["initial_position"], 0);
            assert_eq!(data["observed_steps"], 3);
            let Some(AccountSnapshot::Spot(account)) = rooms
                .participant_observation("f6-batch", "V-BTC-SPOT", 20)
                .unwrap()
                .own_account
            else {
                panic!()
            };
            assert_eq!(account.position_qty, 2);
            assert_eq!(rooms.execution_history("f6-batch").unwrap().len(), 4);
        }
    }

    #[test]
    fn routed_orders_and_real_receipts_survive_partial_action_recovery() {
        let package = Package::new(
            r#"import json,sys
r=json.loads(sys.stdin.readline())
actions=[{'BotOrder':{'instrument_id':instrument,'key':key,'action':{'PlaceLimit':{'side':side,'price_tick':price,'qty':1}}}} for instrument,key,side,price in [('V-BTC-SPOT','spot','Buy',80),('V-BTC-PERP','perp','Sell',120)]]
print(json.dumps(dict(protocol_version=r['protocol_version'],plugin_id=r['plugin_id'],plugin_version=r['plugin_version'],state_version=r['state_version'],actions=actions,state={'decisions':1})))
"#,
            1000,
        );
        let manifest_path = package.0.join("test/bot.json");
        let mut manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["related_instruments_parameter"] = serde_json::json!("markets");
        manifest["bot"]["parameters"]["markets"] =
            serde_json::json!({"type":"array","default":["V-BTC-PERP"]});
        fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        for crash in [
            CrashPoint::AfterDecisionPersist {
                step: 1,
                participant_index: 0,
            },
            CrashPoint::AfterActionSubmit {
                step: 1,
                participant_index: 0,
                action_index: 0,
            },
            CrashPoint::AfterActionSubmit {
                step: 1,
                participant_index: 0,
                action_index: 1,
            },
        ] {
            let mut spec = scenario();
            let fixture: Value = serde_json::from_str(include_str!(
                "../../scripts/fixtures/microstructure_market.json"
            ))
            .unwrap();
            let mut peer = fixture["scenario"]["extra_markets"][0].clone();
            peer["Perp"]["price_link"] = Value::Null;
            peer["Perp"]["funding"] = Value::Null;
            spec.extra_markets
                .push(serde_json::from_value(peer).unwrap());
            let mut rooms = RoomManager::new();
            rooms.create_room(spec).unwrap();
            let mut id = 100;
            let outcome = run_scheduler_step_with_registry(
                &mut rooms,
                &mut id,
                SchedulerState::new("f6-batch", vec![template()], SchedulerMode::Manual),
                crash,
                &package.registry(),
            )
            .unwrap();
            assert!(outcome.crashed);
            let saved =
                serde_json::from_slice(&serde_json::to_vec(&outcome.state).unwrap()).unwrap();
            let resumed = run_scheduler_step_with_registry(
                &mut rooms,
                &mut id,
                saved,
                CrashPoint::None,
                &package.registry(),
            )
            .unwrap();
            for instrument in ["V-BTC-SPOT", "V-BTC-PERP"] {
                let view = rooms
                    .participant_observation("f6-batch", instrument, 20)
                    .unwrap();
                assert_eq!(view.own_orders.len(), 1);
            }
            let PersistedAgentKindState::Plugin { data, .. } = &resumed.state.agents[0].kind_state
            else {
                panic!()
            };
            assert_eq!(data["decisions"], 1);
            assert_eq!(data["_host_receipts"].as_array().unwrap().len(), 2);
            assert_eq!(data["_host_receipts"][0]["order_id"], 100);
            assert_eq!(data["_host_receipts"][1]["order_id"], 101);
            assert_eq!(id, 102);
        }
    }

    #[test]
    fn malformed_mismatched_oversized_and_failed_process_responses_fail_closed() {
        let programs = [
            ("print('not-json')", "invalid response"),
            ("print('{}')", "invalid response"),
            ("print('{}\\n{}')", "invalid response"),
            (
                "import sys; print('failure',file=sys.stderr); sys.exit(9)",
                "exited",
            ),
            ("print('x' * 1048577)", "size limit"),
            (
                "import json,sys; r=json.loads(sys.stdin.readline()); print(json.dumps(dict(protocol_version='bot.v2',plugin_id=r['plugin_id'],plugin_version=r['plugin_version'],state_version=1,actions=[],state=None)))",
                "version mismatch",
            ),
            (
                "import json,sys; r=json.loads(sys.stdin.readline()); print(json.dumps(dict(protocol_version='bot.v1',plugin_id=r['plugin_id'],plugin_version=r['plugin_version'],state_version=1,actions=[{'PlaceMarket':{'side':'Buy','qty':1}}]*65,state=None)))",
                "action limit",
            ),
        ];
        for (program, message) in programs {
            let package = Package::new(program, 1000);
            let template = template();
            let mut bot = package
                .registry()
                .create(&template, &template.initial_state())
                .unwrap();
            let before = bot.snapshot();
            let error = bot.decide(&observation()).unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
            assert_eq!(bot.snapshot(), before);
        }
    }

    fn worker_package(program: &str, workers: usize, timeout_ms: u64) -> Package {
        let package = Package::new(program, timeout_ms);
        let path = package.0.join("test/bot.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["worker_pool"] =
            serde_json::json!({"workers": workers, "command": manifest["command"]});
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        package
    }

    #[test]
    fn event_prefilter_is_opt_in_and_bypasses_book_and_every_tick_plugins() {
        let package = Package::new("", 1000);
        let manifest: ProcessBotManifest =
            serde_json::from_slice(&fs::read(package.0.join("test/bot.json")).unwrap()).unwrap();
        assert!(!manifest.host_event_prefilter);
        let mut factory = ProcessBotFactory {
            manifest,
            directory: package.0.clone(),
            pool: None,
            event_prefilter_enabled: false,
        };
        factory.manifest.decision_trigger = Trigger::Events {
            closed_bars: false,
            book: false,
            account: true,
            interval_ms: None,
        };
        assert!(factory.event_filter().is_none());
        assert!(!factory.supports_instance_reuse());
        factory.event_prefilter_enabled = true;
        assert!(factory.event_filter().unwrap().account);
        assert!(factory.supports_instance_reuse());
        factory.manifest.decision_trigger = Trigger::Events {
            closed_bars: false,
            book: true,
            account: true,
            interval_ms: None,
        };
        assert!(factory.event_filter().is_none());
        assert!(!factory.supports_instance_reuse());
        factory.manifest.decision_trigger = Trigger::EveryTick;
        assert!(factory.event_filter().is_none());
        assert!(!factory.supports_instance_reuse());
    }

    const WORKER: &str = r#"import json,sys,os
for line in sys.stdin:
 e=json.loads(line);r=e['request'];state=r['state'] or {}
 response=dict(protocol_version=r['protocol_version'],plugin_id=r['plugin_id'],plugin_version=r['plugin_version'],state_version=r['state_version'],actions=[],state=dict(calls=state.get('calls',0)+1,pid=os.getpid()))
 print(json.dumps(dict(protocol='bot.worker.v1',id=e['id'],response=response)),flush=True)
"#;

    #[test]
    fn cached_process_bot_releases_payload_and_restores_committed_state() {
        let package = worker_package(WORKER, 1, 5000);
        let registry = package.registry();
        let mut bot = registry
            .create(&template(), &template().initial_state())
            .unwrap();
        bot.decide(&observation()).unwrap();
        let committed = bot.take_snapshot();
        let data = |state: PersistedAgentKindState| match state {
            PersistedAgentKindState::Plugin { data, .. } => data,
            _ => panic!(),
        };
        assert_eq!(data(committed.clone())["calls"], 1);
        assert!(data(bot.snapshot()).is_null());
        bot.prepare_reused_call(&committed).unwrap();
        bot.decide(&observation()).unwrap();
        assert_eq!(data(bot.take_snapshot())["calls"], 2);
        // A rejected speculative result must not leak into the next invocation.
        bot.prepare_reused_call(&committed).unwrap();
        bot.decide(&observation()).unwrap();
        assert_eq!(data(bot.take_snapshot())["calls"], 2);
        let mut mismatched = committed;
        let PersistedAgentKindState::Plugin { state_version, .. } = &mut mismatched else {
            unreachable!()
        };
        *state_version += 1;
        assert!(bot.prepare_reused_call(&mismatched).is_err());
        assert!(data(bot.snapshot()).is_null());
    }

    #[test]
    fn delta_transport_reconstructs_appends_and_rejected_host_state_without_worker_output() {
        let transport_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../bot-plugins/pine-strategy/transport.py");
        let path_literal = serde_json::to_string(&transport_path.to_string_lossy()).unwrap();
        let program = format!(
            "import importlib.util\nspec=importlib.util.spec_from_file_location('transport',{path_literal})\nm=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)\ndecoder=m.RequestDecoder()\n{}",
            WORKER
                .replace("r=e['request']", "r=decoder.decode(e)")
                .replace(
                    "pid=os.getpid()",
                    "pid=os.getpid(),history=state.get('history',[]),delta=('request_delta' in e)"
                )
        );
        let package = worker_package(&program, 1, 5000);
        let path = package.0.join("test/bot.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["worker_pool"]["delta_requests"] = Value::Bool(true);
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let registry = package.registry();
        let mut state = template().initial_state();
        let PersistedAgentKindState::Plugin { data, .. } = &mut state else {
            unreachable!()
        };
        *data = serde_json::json!({"calls":0,"history":(0..500).collect::<Vec<_>>()}).into();
        let mut bot = registry.create(&template(), &state).unwrap();
        bot.decide(&observation()).unwrap();
        let PersistedAgentKindState::Plugin { data: first, .. } = bot.snapshot() else {
            unreachable!()
        };
        assert_eq!(first["delta"], false);
        let mut next = bot.snapshot();
        let PersistedAgentKindState::Plugin { data, .. } = &mut next else {
            unreachable!()
        };
        Arc::make_mut(data)["history"]
            .as_array_mut()
            .unwrap()
            .push(Value::from(500));
        let mut bot = registry.create(&template(), &next).unwrap();
        bot.decide(&observation()).unwrap();
        let PersistedAgentKindState::Plugin { data: second, .. } = bot.snapshot() else {
            unreachable!()
        };
        assert_eq!(second["delta"], true);
        assert_eq!(second["calls"], 2);
        assert_eq!(second["history"].as_array().unwrap().len(), 501);
        // Roll back to the pre-decision durable state; never infer calls=3
        // from a worker's previous response or from its transport cache.
        let mut bot = registry.create(&template(), &state).unwrap();
        bot.decide(&observation()).unwrap();
        let PersistedAgentKindState::Plugin { data: restored, .. } = bot.snapshot() else {
            unreachable!()
        };
        assert_eq!(restored["calls"], 1);
        assert_eq!(restored["history"].as_array().unwrap().len(), 500);
        assert_eq!(restored["pid"], first["pid"]);
    }

    #[test]
    fn affinity_retains_each_instance_process_across_concurrent_waves_and_restore() {
        let package = worker_package(
            &WORKER.replace(
                "e=json.loads(line)",
                "import time;time.sleep(.02);e=json.loads(line)",
            ),
            3,
            5000,
        );
        let path = package.0.join("test/bot.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["worker_pool"]["instance_affinity"] = Value::Bool(true);
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let registry = package.registry();
        let mut saved = Vec::new();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(24));
        let jobs: Vec<_> = (0..24)
            .map(|index| {
                let registry = registry.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let AgentTemplate::Plugin(mut config) = template() else {
                        unreachable!()
                    };
                    config.participant.participant_id = format!("affinity-{index}");
                    let template = AgentTemplate::Plugin(config);
                    let mut bot = registry
                        .create(&template, &template.initial_state())
                        .unwrap();
                    barrier.wait();
                    bot.decide(&observation()).unwrap();
                    (template, bot.snapshot())
                })
            })
            .collect();
        for job in jobs {
            saved.push(job.join().unwrap());
        }
        let pids: std::collections::BTreeSet<_> = saved
            .iter()
            .map(|(_, state)| {
                let PersistedAgentKindState::Plugin { data, .. } = state else {
                    unreachable!()
                };
                data["pid"].as_u64().unwrap()
            })
            .collect();
        assert_eq!(pids.len(), 3);
        // Change completion order and recreate every host runtime. Routing is
        // per instance, not the last idle worker or host runtime allocation.
        let jobs: Vec<_> = saved
            .into_iter()
            .rev()
            .map(|(template, state)| {
                let registry = registry.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut bot = registry.create(&template, &state).unwrap();
                    barrier.wait();
                    bot.decide(&observation()).unwrap();
                    let PersistedAgentKindState::Plugin { data: before, .. } = state else {
                        unreachable!()
                    };
                    let PersistedAgentKindState::Plugin { data: after, .. } = bot.snapshot() else {
                        unreachable!()
                    };
                    assert_eq!(before["pid"], after["pid"]);
                    assert_eq!(after["calls"], 2);
                })
            })
            .collect();
        for job in jobs {
            job.join().unwrap();
        }
    }

    #[test]
    fn persistent_worker_interleaves_instances_and_restores_only_supplied_durable_state() {
        let package = worker_package(WORKER, 1, 1000);
        let registry = package.registry();
        let mut first = registry
            .create(
                &template(),
                &exchange_core::PersistedAgent::from_template(template()).kind_state,
            )
            .unwrap();
        first.decide(&observation()).unwrap();
        let saved = first.snapshot();
        let mut second = registry
            .create(
                &template(),
                &exchange_core::PersistedAgent::from_template(template()).kind_state,
            )
            .unwrap();
        second.decide(&observation()).unwrap();
        let mut restored = registry.create(&template(), &saved).unwrap();
        restored.decide(&observation()).unwrap();
        let data = |state: PersistedAgentKindState| match state {
            PersistedAgentKindState::Plugin { data, .. } => data,
            _ => panic!(),
        };
        let a = data(saved);
        let b = data(second.snapshot());
        let c = data(restored.snapshot());
        assert_eq!(a["pid"], b["pid"]);
        assert_eq!(a["pid"], c["pid"]);
        assert_eq!(b["calls"], 1);
        assert_eq!(c["calls"], 2);
    }

    #[test]
    fn persistent_timeout_and_wrong_response_identity_leave_state_unchanged() {
        for program in [
            "import time;time.sleep(2)",
            &WORKER.replace("id=e['id']", "id=e['id']+1"),
        ] {
            let package = worker_package(program, 1, 100);
            let registry = package.registry();
            let mut bot = registry
                .create(
                    &template(),
                    &exchange_core::PersistedAgent::from_template(template()).kind_state,
                )
                .unwrap();
            let before = bot.snapshot();
            assert!(bot.decide(&observation()).is_err());
            assert_eq!(before, bot.snapshot());
        }
    }

    #[test]
    fn persistent_pool_bounds_processes_under_concurrent_instances() {
        let program = WORKER.replace(
            "e=json.loads(line)",
            "import time;time.sleep(.03);e=json.loads(line)",
        );
        let package = worker_package(&program, 2, 5000);
        let registry = package.registry();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let jobs: Vec<_> = (0..12)
            .map(|_| {
                let registry = registry.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut bot = registry
                        .create(&template(), &template().initial_state())
                        .unwrap();
                    barrier.wait();
                    bot.decide(&observation()).unwrap();
                    let PersistedAgentKindState::Plugin { data, .. } = bot.snapshot() else {
                        panic!()
                    };
                    assert_eq!(data["calls"], 1);
                    data["pid"].as_u64().unwrap()
                })
            })
            .collect();
        let pids: std::collections::BTreeSet<_> =
            jobs.into_iter().map(|job| job.join().unwrap()).collect();
        assert_eq!(pids.len(), 2);
    }

    #[test]
    fn persistent_invalid_inner_identity_and_unbounded_output_fail_closed() {
        for program in [
            WORKER.replace("plugin_id=r['plugin_id']", "plugin_id='wrong'"),
            "print('x' * 1048577,flush=True)".into(),
            "import sys,time;sys.stderr.write('x'*65537);sys.stderr.flush();time.sleep(2)".into(),
        ] {
            let package = worker_package(&program, 1, 1000);
            let registry = package.registry();
            let mut bot = registry
                .create(&template(), &template().initial_state())
                .unwrap();
            let before = bot.snapshot();
            assert!(bot.decide(&observation()).is_err());
            assert_eq!(before, bot.snapshot());
        }
    }

    #[test]
    fn event_driven_process_never_starts_for_unchanged_input_after_restore() {
        let package = Package::new(
            r#"import json,sys
r=json.loads(sys.stdin.readline());print(json.dumps(dict(protocol_version=r['protocol_version'],plugin_id=r['plugin_id'],plugin_version=r['plugin_version'],state_version=r['state_version'],actions=[],state=dict(calls=(r['state'] or {}).get('calls',0)+1))))
"#,
            1000,
        );
        let path = package.0.join("test/bot.json");
        let mut manifest: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest["decision_trigger"] =
            serde_json::json!({"kind":"events","book":true,"account":true});
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let registry = package.registry();
        let mut bot = registry
            .create(
                &template(),
                &exchange_core::PersistedAgent::from_template(template()).kind_state,
            )
            .unwrap();
        let mut view = observation();
        bot.decide(&view).unwrap();
        let saved = bot.snapshot();
        fs::write(
            package.0.join("test/bot.py"),
            "raise RuntimeError('must not execute')",
        )
        .unwrap();
        let registry = package.registry();
        let mut restored = registry.create(&template(), &saved).unwrap();
        view.market_time_ms += 1000;
        view.step += 1;
        assert!(
            !registry
                .decision_is_due(&template(), &saved, &view)
                .unwrap()
        );
        assert!(restored.decide(&view).unwrap().is_empty());
        assert_eq!(restored.snapshot(), saved);
        view.book.asks.clear();
        assert!(restored.decide(&view).is_err());
    }

    #[test]
    fn timeout_reaps_process_and_does_not_change_state() {
        let package = Package::new("import time; time.sleep(5)", 100);
        let template = template();
        let mut bot = package
            .registry()
            .create(&template, &template.initial_state())
            .unwrap();
        let started = Instant::now();
        assert!(
            bot.decide(&observation())
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(bot.snapshot(), template.initial_state());
    }

    #[test]
    fn package_duplicate_ids_invalid_parameters_and_versions_are_rejected() {
        let package = Package::new("print('{}')", 1000);
        let registry = package.registry();
        let mut config = match template() {
            AgentTemplate::Plugin(config) => config,
            _ => unreachable!(),
        };
        config.config = serde_json::json!({"target_qty":0});
        assert!(
            registry
                .validate_template(&AgentTemplate::Plugin(config.clone()))
                .is_err()
        );
        config.config = serde_json::json!({"target_qty":2});
        config.plugin_version = "2".into();
        assert!(
            registry
                .validate_template(&AgentTemplate::Plugin(config))
                .is_err()
        );
        fs::create_dir(package.0.join("duplicate")).unwrap();
        fs::copy(
            package.0.join("test/bot.json"),
            package.0.join("duplicate/bot.json"),
        )
        .unwrap();
        assert!(
            load_bot_plugins(&package.0)
                .err()
                .unwrap()
                .to_string()
                .contains("duplicate")
        );
    }
}
