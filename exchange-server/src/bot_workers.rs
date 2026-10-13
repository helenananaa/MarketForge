//! Bounded persistent bot.worker.v1 processes. No speculative state survives in the host.
use super::bot_plugins::{
    MAX_MESSAGE_BYTES, MAX_REQUEST_BYTES, MAX_STDERR_BYTES, ManagedChild, ProcessBotManifest,
    bot_io, process_command,
};
use super::bot_wire::RequestCache;
use exchange_core::{BotDecisionRequest, BotDecisionResponse, BotError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkerConfig {
    pub workers: usize,
    pub command: Vec<String>,
    #[serde(default)]
    pub instance_affinity: bool,
    #[serde(default = "default_max_calls")]
    pub max_calls: usize,
    #[serde(default)]
    pub delta_requests: bool,
}
fn default_max_calls() -> usize {
    10_000
}
impl WorkerConfig {
    pub fn validate(&self) -> Result<(), BotError> {
        if !(1..=32).contains(&self.workers)
            || self.command.is_empty()
            || self.command[0].is_empty()
            || !(1..=1_000_000).contains(&self.max_calls)
        {
            return Err(BotError("invalid persistent worker configuration".into()));
        }
        Ok(())
    }
}

enum Message {
    Response(Vec<u8>),
    Error(String),
}
struct Worker {
    child: ManagedChild,
    input: mpsc::SyncSender<Vec<u8>>,
    output: mpsc::Receiver<Message>,
    calls: usize,
    requests: Option<RequestCache>,
}
impl Worker {
    fn start(directory: &Path, config: &WorkerConfig) -> Result<Self, BotError> {
        #[cfg(feature = "bench-execution-timing")]
        let _timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::BOT_WORKER_START,
        );
        let mut child = ManagedChild(
            process_command(directory, &config.command)
                // First-party workers may return non-authoritative runtime
                // diagnostics. Old hosts do not request this envelope extension.
                .env("MARKETFORGE_BOT_WORKER_DIAGNOSTICS", "1")
                .spawn()
                .map_err(bot_io)?,
        );
        let mut stdin = child.0.stdin.take().unwrap();
        let stdout = child.0.stdout.take().unwrap();
        let mut stderr = child.0.stderr.take().unwrap();
        let (input, writes) = mpsc::sync_channel::<Vec<u8>>(1);
        let (messages, output) = mpsc::sync_channel(2);
        let errors = messages.clone();
        std::thread::spawn(move || {
            while let Ok(bytes) = writes.recv() {
                if let Err(e) = stdin.write_all(&bytes).and_then(|_| stdin.flush()) {
                    let _ = errors.send(Message::Error(e.to_string()));
                    break;
                }
            }
        });
        let errors = messages.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut bytes = Vec::new();
                let result = reader
                    .by_ref()
                    .take((MAX_MESSAGE_BYTES + 1) as u64)
                    .read_until(b'\n', &mut bytes);
                match result {
                    Ok(0) => {
                        let _ = messages.send(Message::Error("worker closed stdout".into()));
                        break;
                    }
                    Ok(_) if bytes.len() > MAX_MESSAGE_BYTES || bytes.last() != Some(&b'\n') => {
                        let _ = messages.send(Message::Error(
                            "worker response exceeds framing limit".into(),
                        ));
                        break;
                    }
                    Ok(_) => {
                        if messages.send(Message::Response(bytes)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = messages.send(Message::Error(e.to_string()));
                        break;
                    }
                }
            }
        });
        std::thread::spawn(move || {
            // Lifetime cap, not an unbounded diagnostics buffer. Quiet workers have
            // one blocking reader; exceeding the cap invalidates and reaps the worker.
            let mut size = 0;
            let mut buf = [0u8; 4096];
            loop {
                match stderr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        size += n;
                        if size > MAX_STDERR_BYTES {
                            let _ = errors.send(Message::Error(
                                "worker stderr exceeds lifetime limit".into(),
                            ));
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            child,
            input,
            output,
            calls: 0,
            requests: config.delta_requests.then(RequestCache::default),
        })
    }
    fn exchange(
        &mut self,
        request: &BotDecisionRequest,
        deadline: Instant,
    ) -> Result<BotDecisionResponse, BotError> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(1);
        let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        #[cfg(feature = "bench-execution-timing")]
        let encode_timer = super::bench_execution_timing::Timer::start(
            &super::bench_execution_timing::BOT_WIRE_ENCODE,
        );
        let mut packet = self
            .requests
            .as_ref()
            .map(|cache| cache.encode(request, id, MAX_REQUEST_BYTES))
            .transpose()?;
        let bytes = if let Some(packet) = &mut packet {
            std::mem::take(&mut packet.bytes)
        } else {
            let mut bytes = serde_json::to_vec(
                &serde_json::json!({"protocol":"bot.worker.v1", "id":id, "request":request}),
            )
            .map_err(bot_io)?;
            bytes.push(b'\n');
            bytes
        };
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(BotError("worker request exceeds size limit".into()));
        }
        #[cfg(feature = "bench-execution-timing")]
        drop(encode_timer);
        #[cfg(feature = "bench-execution-timing")]
        let roundtrip_timer = super::bench_execution_timing::Timer::start(
            &super::bench_execution_timing::BOT_WORKER_ROUNDTRIP,
        );
        self.input
            .try_send(bytes)
            .map_err(|_| BotError("worker input unavailable".into()))?;
        let message = self
            .output
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| BotError("persistent bot worker timed out".into()))?;
        let Message::Response(bytes) = message else {
            let Message::Error(error) = message else {
                unreachable!()
            };
            return Err(BotError(error));
        };
        #[cfg(feature = "bench-execution-timing")]
        {
            crate::bench_execution_timing::trace(
                "worker_response",
                &request.participant.participant_id,
                request.observation.market_time_ms,
                roundtrip_timer.1.elapsed().as_micros() as u64,
            );
            drop(roundtrip_timer);
        }
        #[cfg(feature = "bench-execution-timing")]
        let _decode_timer = super::bench_execution_timing::Timer::start(
            &super::bench_execution_timing::BOT_WIRE_DECODE,
        );
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Reply {
            protocol: String,
            id: u64,
            response: Option<BotDecisionResponse>,
            error: Option<String>,
            runtime_profile: Option<serde_json::Value>,
        }
        let reply: Reply = serde_json::from_slice(&bytes).map_err(bot_io)?;
        if reply.protocol != "bot.worker.v1" || reply.id != id {
            return Err(BotError("worker response identity mismatch".into()));
        }
        match (reply.response, reply.error) {
            (Some(response), None) => {
                if response.protocol_version != request.protocol_version
                    || response.plugin_id != request.plugin_id
                    || response.plugin_version != request.plugin_version
                    || response.state_version != request.state_version
                    || response.actions.len() > exchange_core::MAX_BOT_ACTIONS
                {
                    return Err(BotError(
                        "worker decision identity/action bounds mismatch".into(),
                    ));
                }
                self.calls += 1;
                if let Some(packet) = packet {
                    self.requests.as_mut().unwrap().accept(packet);
                }
                if let Some(profile) = reply.runtime_profile {
                    crate::bot_runtime_diagnostics::record_evaluation(
                        "process",
                        profile
                            .get("mode")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown"),
                        profile.get("reason").and_then(serde_json::Value::as_str),
                    );
                } else {
                    crate::bot_runtime_diagnostics::record_evaluation(
                        "process",
                        "unknown",
                        Some("worker_unreported"),
                    );
                }
                Ok(response)
            }
            (None, Some(error)) => Err(BotError(format!("worker decision failed: {error}"))),
            _ => Err(BotError("invalid worker response envelope".into())),
        }
    }
}

#[derive(Default)]
struct Slots {
    idle: VecDeque<(u64, usize, Worker)>,
    occupied: BTreeSet<(u64, usize)>,
    counts: BTreeMap<u64, usize>,
    total: usize,
}
impl Slots {
    fn release(&mut self, owner: u64, shard: usize) {
        assert!(self.occupied.remove(&(owner, shard)));
        let count = self.counts.get_mut(&owner).unwrap();
        *count -= 1;
        if *count == 0 {
            self.counts.remove(&owner);
        }
        self.total -= 1;
    }
}
const MAX_SERVICE_WORKERS: usize = 32;
#[derive(Default)]
struct WorkerHub {
    slots: Mutex<Slots>,
    available: Condvar,
}
pub(super) struct WorkerPool {
    config: WorkerConfig,
    id: u64,
    hub: Arc<WorkerHub>,
}
impl WorkerPool {
    pub fn new(config: WorkerConfig) -> Arc<Self> {
        static HUB: OnceLock<Arc<WorkerHub>> = OnceLock::new();
        Self::with_hub(config, HUB.get_or_init(Default::default).clone())
    }
    fn with_hub(config: WorkerConfig, hub: Arc<WorkerHub>) -> Arc<Self> {
        static POOLS: AtomicU64 = AtomicU64::new(1);
        Arc::new(Self {
            config,
            id: POOLS.fetch_add(1, Ordering::Relaxed),
            hub,
        })
    }
    pub fn exchange(
        &self,
        directory: &Path,
        manifest: &ProcessBotManifest,
        request: &BotDecisionRequest,
    ) -> Result<BotDecisionResponse, BotError> {
        let deadline = Instant::now() + Duration::from_millis(manifest.timeout_ms);
        #[cfg(feature = "bench-execution-timing")]
        let acquire_timer = super::bench_execution_timing::Timer::start(
            &super::bench_execution_timing::BOT_WORKER_ACQUIRE,
        );
        // Stable across requests/restarts; no state or configuration controls routing.
        let target = if self.config.instance_affinity {
            let identity = serde_json::to_vec(&request.participant).map_err(bot_io)?;
            let hash = Sha256::digest(identity);
            Some(
                (u64::from_le_bytes(hash[..8].try_into().unwrap()) % self.config.workers as u64)
                    as usize,
            )
        } else {
            None
        };
        let mut slots = self.hub.slots.lock().map_err(bot_io)?;
        let (shard, mut worker) = loop {
            // A busy affinity shard queues within the same deadline instead of
            // migrating the bot and rebuilding its session in another process.
            if let Some(index) = slots.idle.iter().rposition(|(owner, shard, _)| {
                *owner == self.id && target.is_none_or(|target| target == *shard)
            }) {
                let (_, shard, worker) = slots.idle.remove(index).unwrap();
                drop(slots);
                break (shard, worker);
            }
            let vacant = target
                .filter(|shard| !slots.occupied.contains(&(self.id, *shard)))
                .or_else(|| {
                    if target.is_some() {
                        None
                    } else {
                        (0..self.config.workers)
                            .find(|shard| !slots.occupied.contains(&(self.id, *shard)))
                    }
                });
            if let Some(shard) = vacant
                && (slots.total < MAX_SERVICE_WORKERS || !slots.idle.is_empty())
            {
                let evicted = if slots.total == MAX_SERVICE_WORKERS {
                    let (owner, old_shard, worker) = slots.idle.pop_front().unwrap();
                    slots.release(owner, old_shard);
                    Some(worker)
                } else {
                    None
                };
                slots.total += 1;
                slots.occupied.insert((self.id, shard));
                *slots.counts.entry(self.id).or_default() += 1;
                drop(slots);
                // Reaping can block; never hold the service mutex while doing so.
                drop(evicted);
                match Worker::start(directory, &self.config) {
                    Ok(worker) => break (shard, worker),
                    Err(error) => {
                        self.hub.slots.lock().unwrap().release(self.id, shard);
                        self.hub.available.notify_all();
                        return Err(error);
                    }
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(BotError("persistent worker queue timed out".into()));
            }
            let (next, _) = self
                .hub
                .available
                .wait_timeout(slots, remaining)
                .map_err(bot_io)?;
            slots = next;
        };
        #[cfg(feature = "bench-execution-timing")]
        {
            crate::bench_execution_timing::trace(
                "worker_acquired",
                &request.participant.participant_id,
                request.observation.market_time_ms,
                acquire_timer.1.elapsed().as_micros() as u64,
            );
            drop(acquire_timer);
        }
        let result = worker.exchange(request, deadline);
        // A failed or aging process is never retried speculatively. Host state is
        // unchanged on errors; a subsequent caller starts a clean worker.
        let reusable = result.is_ok()
            && worker.calls < self.config.max_calls
            && worker.child.0.try_wait().ok() == Some(None);
        let mut slots = self.hub.slots.lock().map_err(bot_io)?;
        if reusable {
            slots.idle.push_back((self.id, shard, worker));
            drop(slots);
        } else {
            drop(slots);
            drop(worker);
            self.hub
                .slots
                .lock()
                .map_err(bot_io)?
                .release(self.id, shard);
        }
        // Different plugin pools share the condition variable; waking only one
        // waiter could leave an eligible pool asleep behind a busy pool.
        self.hub.available.notify_all();
        result
    }
}
impl Drop for WorkerPool {
    fn drop(&mut self) {
        let mut slots = self.hub.slots.lock().unwrap();
        let mut retired = Vec::new();
        let mut index = 0;
        while index < slots.idle.len() {
            if slots.idle[index].0 == self.id {
                let (_, shard, worker) = slots.idle.remove(index).unwrap();
                retired.push((shard, worker));
            } else {
                index += 1;
            }
        }
        drop(slots);
        let shards: Vec<_> = retired.iter().map(|(shard, _)| *shard).collect();
        drop(retired);
        let mut slots = self.hub.slots.lock().unwrap();
        for shard in shards {
            slots.release(self.id, shard);
        }
        drop(slots);
        self.hub.available.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange_core::{ParticipantConfig, ParticipantKind, RoomManager};

    #[test]
    fn service_budget_evicts_idle_plugins_and_rebuilds_from_durable_state() {
        let hub = Arc::new(WorkerHub::default());
        let directory =
            std::env::temp_dir().join(format!("marketforge-worker-budget-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("bot.py"), r#"import json,sys,os
for line in sys.stdin:
 e=json.loads(line);r=e['request'];s=r['state'] or {}
 response=dict(protocol_version=r['protocol_version'],plugin_id=r['plugin_id'],plugin_version=r['plugin_version'],state_version=r['state_version'],actions=[],state=dict(calls=s.get('calls',0)+1,pid=os.getpid()))
 print(json.dumps(dict(protocol='bot.worker.v1',id=e['id'],response=response)),flush=True)
"#).unwrap();
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../../scripts/fixtures/f6_batch_spec.json"))
                .unwrap();
        let mut rooms = RoomManager::new();
        rooms
            .create_room(serde_json::from_value(fixture["scenario"].clone()).unwrap())
            .unwrap();
        let mut manifest: ProcessBotManifest =
            serde_json::from_str(include_str!("../../bot-plugins/buy-remaining/bot.json")).unwrap();
        manifest.timeout_ms = 5000;
        let config = WorkerConfig {
            workers: 1,
            command: manifest.command.clone(),
            instance_affinity: false,
            max_calls: default_max_calls(),
            delta_requests: false,
        };
        let mut request = BotDecisionRequest {
            protocol_version: "bot.v1".into(),
            plugin_id: manifest.bot.id.clone(),
            plugin_version: manifest.bot.version.clone(),
            state_version: 1,
            participant: ParticipantConfig {
                participant_id: "buyer".into(),
                kind: ParticipantKind::RuleAgent,
                room_id: "f6-batch".into(),
                account_id: 20,
                instrument_id: Some("V-BTC-SPOT".into()),
            },
            seed: 7,
            config: serde_json::json!({}),
            state: serde_json::json!({}),
            observation: rooms
                .participant_observation("f6-batch", "V-BTC-SPOT", 20)
                .unwrap(),
        };
        let mut pools = Vec::new();
        let mut saved = None;
        for index in 0..MAX_SERVICE_WORKERS + 3 {
            let pool = WorkerPool::with_hub(config.clone(), hub.clone());
            let response = pool.exchange(&directory, &manifest, &request).unwrap();
            if index == 0 {
                saved = Some(response.state);
            }
            pools.push(pool);
            let slots = hub.slots.lock().unwrap();
            assert_eq!(slots.total, (index + 1).min(MAX_SERVICE_WORKERS));
            assert_eq!(slots.idle.len(), slots.total);
        }
        assert!(!hub.slots.lock().unwrap().counts.contains_key(&pools[0].id));
        request.state = saved.unwrap();
        let restored = pools[0].exchange(&directory, &manifest, &request).unwrap();
        assert_eq!(restored.state["calls"], 2);
        assert_ne!(restored.state["pid"], request.state["pid"]);
        assert_eq!(hub.slots.lock().unwrap().total, MAX_SERVICE_WORKERS);
        drop(pools);
        assert_eq!(hub.slots.lock().unwrap().total, 0);
        assert!(hub.slots.lock().unwrap().counts.is_empty());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
