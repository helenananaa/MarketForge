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

use exchange_core::{
    AgentTemplate, BOT_PROTOCOL_VERSION, BotConfig, BotDecisionRequest, BotDecisionResponse,
    BotDescriptor, BotError, BotFactory, BotRegistry, MAX_BOT_ACTIONS, OrderAction,
    ParticipantObservation, PersistedAgentKindState, ScheduledBot,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const BOT_PLUGIN_DIR_ENV: &str = "MARKETFORGE_BOT_PLUGIN_DIR";
const MAX_MESSAGE_BYTES: usize = 1_048_576;
const MAX_REQUEST_BYTES: usize = 4 * MAX_MESSAGE_BYTES;
const MAX_STDERR_BYTES: usize = 65_536;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessBotManifest {
    pub bot: BotDescriptor,
    pub command: Vec<String>,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_data: Option<ProcessMarketData>,
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
        registry.register(ProcessBotFactory {
            manifest,
            directory,
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
    fn descriptor(&self) -> &BotDescriptor {
        &self.manifest.bot
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
    state: Value,
}
impl ScheduledBot for ProcessBot {
    fn decide(
        &mut self,
        observation: &ParticipantObservation,
    ) -> Result<Vec<OrderAction>, BotError> {
        let request = BotDecisionRequest {
            protocol_version: BOT_PROTOCOL_VERSION.into(),
            plugin_id: self.config.plugin_id.clone(),
            plugin_version: self.config.plugin_version.clone(),
            state_version: self.config.state_version,
            participant: self.config.participant.clone(),
            seed: self.config.seed,
            config: self.config.config.clone(),
            state: self.state.clone(),
            observation: observation.clone(),
        };
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
        self.state = response.state;
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
}

fn bot_io(error: impl std::fmt::Display) -> BotError {
    BotError(error.to_string())
}

/// Kill/reap even when an IO error or malformed message ends the exchange.
struct ManagedChild(Child);
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

impl ProcessBotFactory {
    fn exchange(&self, request: &BotDecisionRequest) -> Result<BotDecisionResponse, BotError> {
        let mut input = serde_json::to_vec(request).map_err(bot_io)?;
        input.push(b'\n');
        if input.len() > MAX_REQUEST_BYTES {
            return Err(BotError("bot request exceeds size limit".into()));
        }
        let mut command = Command::new(&self.manifest.command[0]);
        command
            .args(&self.manifest.command[1..])
            .current_dir(&self.directory)
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
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
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
