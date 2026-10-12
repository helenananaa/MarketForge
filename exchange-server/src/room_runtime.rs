//! Room-owned exchange execution. Ordinary orders prepare in venue FIFO lanes
//! and share a short room commit lane. Clock commands and bot intents exclude
//! venue preparation with a room coordination barrier; rooms remain independent.
//! A directory barrier coordinates rare operations which change global identity,
//! competition configuration, room ownership or the set of rooms. It does not
//! serialize ordinary room transactions. Committed immutable room snapshots are
//! published separately so market readers do not wait for matching or journal I/O.
use super::*;
use std::ops::{Deref, DerefMut};
use tokio::sync::{OwnedMutexGuard, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

pub(super) struct StateStore {
    directory: Arc<RwLock<Directory>>,
}

struct Directory {
    global: AppState,
    rooms: BTreeMap<RoomId, Arc<RoomRuntime>>,
}

struct RoomRuntime {
    state: Arc<AsyncMutex<Option<AppState>>>,
    coordination: Arc<RwLock<()>>,
    venues: std::sync::RwLock<BTreeMap<String, Arc<AsyncMutex<()>>>>,
    published: std::sync::RwLock<PublishedRoom>,
    summary: std::sync::RwLock<RoomSummary>,
}

#[derive(Default)]
pub(super) struct RoomSummary {
    has_training: bool,
    pub(super) room_count: usize,
    pub(super) worker_count: usize,
    pub(super) cache_entries: usize,
    pub(super) owned_leases: usize,
    pub(super) lost_leases: usize,
    pub(super) renew_failures: u64,
    pub(super) training_running: usize,
    pub(super) training_completed: usize,
    pub(super) training_failed: usize,
    pub(super) agent_errors: usize,
    pub(super) lease_error: Option<String>,
    worker_errors: Vec<Arc<Mutex<Option<String>>>>,
}

impl RoomSummary {
    fn from_app(app: &AppState) -> Self {
        let (owned_leases, lost_leases, renew_failures) = app.room_lease_metrics();
        let mut summary = Self {
            has_training: !app.training_runs.is_empty(),
            room_count: app.rooms.room_ids().len(),
            worker_count: app.agent_workers.len(),
            cache_entries: app.executions.values().map(VecDeque::len).sum(),
            owned_leases,
            lost_leases,
            renew_failures,
            lease_error: app.room_lease_readiness_error(),
            worker_errors: app
                .agent_workers
                .values()
                .map(|worker| worker.last_error.clone())
                .collect(),
            ..Self::default()
        };
        for run in app.training_runs.values() {
            match run.status {
                TrainingStatus::Completed => summary.training_completed += 1,
                TrainingStatus::Failed | TrainingStatus::Aborted => summary.training_failed += 1,
                TrainingStatus::Created | TrainingStatus::Running => summary.training_running += 1,
            }
        }
        summary
    }

    fn add(&mut self, other: &Self) {
        self.room_count += other.room_count;
        self.worker_count += other.worker_count;
        self.cache_entries += other.cache_entries;
        self.owned_leases += other.owned_leases;
        self.lost_leases += other.lost_leases;
        self.renew_failures = self.renew_failures.saturating_add(other.renew_failures);
        self.training_running += other.training_running;
        self.training_completed += other.training_completed;
        self.training_failed += other.training_failed;
        self.agent_errors += other
            .worker_errors
            .iter()
            .filter(|error| error.lock().unwrap().is_some())
            .count();
        if self.lease_error.is_none() {
            self.lease_error = other.lease_error.clone();
        }
    }
}

pub(super) enum StateGuard {
    Global(GlobalGuard),
    Room(RoomGuard),
}

pub(super) struct GlobalGuard {
    directory: OwnedRwLockWriteGuard<Directory>,
}

pub(super) struct RoomGuard {
    // Drop the room state before releasing the directory barrier.
    state: OwnedMutexGuard<Option<AppState>>,
    runtime: Arc<RoomRuntime>,
    _coordination: Option<OwnedRwLockWriteGuard<()>>,
    _directory: Arc<OwnedRwLockReadGuard<Directory>>,
    changed: bool,
    market_changed: bool,
    dispatch_patch: Option<(u64, Vec<usize>)>,
    #[cfg(feature = "bench-execution-timing")]
    timing_started: Instant,
}

pub(super) struct MetadataGuard(OwnedRwLockReadGuard<Directory>);

pub(super) struct RoomReadState {
    pub(super) rooms: Arc<RoomManager>,
}

/// Market and scheduler predecessors are published together, only after durable
/// installation. Readers never acquire the writer lane or see candidate state.
#[derive(Clone)]
pub(super) struct PublishedRoom {
    pub(super) rooms: Arc<RoomManager>,
    pub(super) dispatch: Option<Arc<DispatchState>>,
}

pub(super) struct DispatchState {
    pub(super) roster: Arc<()>,
    pub(super) revision: u64,
    pub(super) bots_enabled: bool,
    // Persistent vector nodes share immutable agents. Updating one slot must
    // not deep-copy the neighboring bots in the same vector chunk.
    pub(super) agents: im::Vector<Arc<exchange_core::PersistedAgent>>,
    pub(super) registry: exchange_core::BotRegistry,
}

impl DispatchState {
    fn publish(
        app: &AppState,
        previous: Option<&Arc<Self>>,
        patch: Option<&(u64, Vec<usize>)>,
    ) -> Option<Arc<Self>> {
        let scheduler = app.schedulers.values().next()?;
        if let Some(previous) = previous
            && previous.revision == scheduler.revision
            && previous.bots_enabled == scheduler.bots_enabled
        {
            return Some(previous.clone());
        }
        let (agents, roster) = match (previous, patch) {
            (Some(previous), Some((base, indices)))
                if previous.revision == *base
                    && previous.agents.len() == scheduler.agents.len() =>
            {
                let mut agents = previous.agents.clone();
                for &index in indices {
                    agents.set(index, Arc::new(scheduler.agents[index].clone()));
                }
                (agents, previous.roster.clone())
            }
            _ => (
                scheduler.agents.iter().cloned().map(Arc::new).collect(),
                Arc::new(()),
            ),
        };
        Some(Arc::new(Self {
            roster,
            revision: scheduler.revision,
            bots_enabled: scheduler.bots_enabled,
            agents,
            registry: app.bot_registry.clone(),
        }))
    }
}

/// Venue FIFO protects shared collateral while computation is outside the
/// room commit mutex. The reader barrier excludes clock, transfer, bot,
/// automation and administrative transactions until publication completes.
pub(super) struct VenueOrderGuard {
    runtime: Arc<RoomRuntime>,
    _venue: OwnedMutexGuard<()>,
    _coordination: OwnedRwLockReadGuard<()>,
    directory: Arc<OwnedRwLockReadGuard<Directory>>,
    pub(super) predecessor: Arc<RoomManager>,
    pub(super) instrument: String,
}

impl VenueOrderGuard {
    pub(super) fn metadata(&self) -> &AppState {
        &self.directory.global
    }

    pub(super) async fn commit_lock(&self) -> StateGuard {
        #[cfg(feature = "bench-execution-timing")]
        let started = Instant::now();
        let state = self.runtime.state.clone().lock_owned().await;
        #[cfg(feature = "bench-execution-timing")]
        bench_execution_timing::ROOM_WAIT.record(started);
        StateGuard::Room(RoomGuard {
            state,
            runtime: self.runtime.clone(),
            _coordination: None,
            _directory: self.directory.clone(),
            changed: false,
            market_changed: false,
            dispatch_patch: None,
            #[cfg(feature = "bench-execution-timing")]
            timing_started: Instant::now(),
        })
    }
}

impl Deref for MetadataGuard {
    type Target = AppState;
    fn deref(&self) -> &AppState {
        &self.0.global
    }
}

impl StateStore {
    /// Read-only admission I/O runs behind the directory fence, but before
    /// taking the room writer/commit lane. No membership update can interleave.
    pub(super) async fn order_admission(
        &self,
        room: &str,
        user: &str,
    ) -> Result<(StateGuard, bool), JournalError> {
        let directory = Arc::new(self.directory.clone().read_owned().await);
        if let Some(runtime) = directory.rooms.get(room).cloned() {
            let admin = directory
                .global
                .journal
                .user_can_administer_room(user, room)
                .await?;
            #[cfg(feature = "bench-execution-timing")]
            let started = Instant::now();
            let coordination = runtime.coordination.clone().write_owned().await;
            let state = runtime.state.clone().lock_owned().await;
            #[cfg(feature = "bench-execution-timing")]
            bench_execution_timing::ROOM_WAIT.record(started);
            return Ok((
                StateGuard::Room(RoomGuard {
                    state,
                    runtime,
                    _coordination: Some(coordination),
                    _directory: directory,
                    changed: false,
                    market_changed: false,
                    dispatch_patch: None,
                    #[cfg(feature = "bench-execution-timing")]
                    timing_started: Instant::now(),
                }),
                admin,
            ));
        }
        drop(directory);
        let state = self.lock().await;
        let admin = state.journal.user_can_administer_room(user, room).await?;
        Ok((state, admin))
    }

    pub(super) async fn venue_order(
        &self,
        room: &str,
        instrument: Option<&str>,
    ) -> Option<VenueOrderGuard> {
        #[cfg(feature = "bench-execution-timing")]
        let started = Instant::now();
        let directory = Arc::new(self.directory.clone().read_owned().await);
        #[cfg(feature = "bench-execution-timing")]
        bench_execution_timing::DIRECTORY_WAIT.record(started);
        let runtime = directory.rooms.get(room)?.clone();
        let coordination = runtime.coordination.clone().read_owned().await;
        if runtime.summary.read().unwrap().has_training {
            return None;
        }
        let snapshot = runtime.published.read().unwrap().rooms.clone();
        let simulation = snapshot.simulation_room(room).ok()?;
        if simulation.venue_ids().len() < 2 {
            return None;
        }
        let instrument = instrument.map(str::to_owned).unwrap_or_else(|| {
            simulation
                .primary_exchange()
                .primary_instrument_id()
                .to_owned()
        });
        let venue = simulation.venue_id_for_instrument(&instrument)?;
        let lane = runtime.venues.read().unwrap().get(&venue)?.clone();
        #[cfg(feature = "bench-execution-timing")]
        let started = Instant::now();
        let venue_guard = lane.lock_owned().await;
        #[cfg(feature = "bench-execution-timing")]
        bench_execution_timing::VENUE_WAIT.record(started);
        // Take the snapshot after acquiring the venue lane, so its accounts and
        // books include the preceding venue transaction's committed result.
        let predecessor = runtime.published.read().unwrap().rooms.clone();
        Some(VenueOrderGuard {
            runtime,
            _venue: venue_guard,
            _coordination: coordination,
            directory,
            predecessor,
            instrument,
        })
    }

    pub(super) fn new(mut app: AppState) -> Self {
        app.order_ids = Some(Arc::new(AtomicU64::new(app.next_order_id)));
        let mut directory = Directory {
            global: app,
            rooms: BTreeMap::new(),
        };
        directory.partition();
        Self {
            directory: Arc::new(RwLock::new(directory)),
        }
    }

    pub(super) async fn metadata(&self) -> MetadataGuard {
        MetadataGuard(self.directory.clone().read_owned().await)
    }

    pub(super) async fn summary(&self) -> (RoomSummary, JournalCoordinator) {
        let directory = self.directory.read().await;
        let mut summary = RoomSummary::from_app(&directory.global);
        for runtime in directory.rooms.values() {
            summary.add(&runtime.summary.read().unwrap());
        }
        (summary, directory.global.journal.clone())
    }

    pub(super) async fn room_ids(&self) -> Vec<String> {
        let directory = self.directory.read().await;
        directory
            .rooms
            .iter()
            .filter(|(id, room)| room.published.read().unwrap().rooms.status(id).is_ok())
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub(super) async fn read_room(&self, room: &str) -> RoomReadState {
        let directory = self.directory.read().await;
        let rooms = directory
            .rooms
            .get(room)
            .map(|runtime| runtime.published.read().unwrap().rooms.clone())
            .unwrap_or_else(|| Arc::new(RoomManager::new()));
        RoomReadState { rooms }
    }

    pub(super) async fn read_dispatch(&self, room: &str) -> Option<PublishedRoom> {
        let directory = self.directory.read().await;
        directory
            .rooms
            .get(room)
            .map(|runtime| runtime.published.read().unwrap().clone())
    }

    pub(super) async fn lock_room(&self, room: &str) -> StateGuard {
        #[cfg(feature = "bench-execution-timing")]
        let directory_started = Instant::now();
        let directory = self.directory.clone().read_owned().await;
        #[cfg(feature = "bench-execution-timing")]
        bench_execution_timing::DIRECTORY_WAIT.record(directory_started);
        if let Some(runtime) = directory.rooms.get(room).cloned() {
            #[cfg(feature = "bench-execution-timing")]
            let lock_started = Instant::now();
            let coordination = runtime.coordination.clone().write_owned().await;
            let state = runtime.state.clone().lock_owned().await;
            #[cfg(feature = "bench-execution-timing")]
            bench_execution_timing::ROOM_WAIT.record(lock_started);
            return StateGuard::Room(RoomGuard {
                state,
                runtime,
                _coordination: Some(coordination),
                _directory: Arc::new(directory),
                changed: false,
                market_changed: false,
                dispatch_patch: None,
                #[cfg(feature = "bench-execution-timing")]
                timing_started: Instant::now(),
            });
        }
        // Creation and lazy leased-room acquisition must coordinate the directory.
        drop(directory);
        self.lock().await
    }

    pub(super) async fn lock(&self) -> StateGuard {
        let mut directory = self.directory.clone().write_owned().await;
        // Every room guard holds a directory reader. Once the writer barrier is
        // acquired, no room is executing and its state can be moved, not copied.
        let runtimes: Vec<_> = directory.rooms.values().cloned().collect();
        for runtime in runtimes {
            let mut slot = runtime
                .state
                .try_lock()
                .expect("directory exclusively owns room lanes");
            if let Some(app) = slot.take() {
                directory.global.absorb_room(app);
            }
        }
        directory.global.next_order_id = directory
            .global
            .order_ids
            .as_ref()
            .unwrap()
            .load(Ordering::Acquire);
        StateGuard::Global(GlobalGuard { directory })
    }
}

impl Directory {
    fn partition(&mut self) {
        let ids: BTreeSet<String> = self
            .global
            .rooms
            .room_ids()
            .into_iter()
            .map(str::to_owned)
            .chain(self.global.schedulers.keys().cloned())
            .chain(self.global.agent_workers.keys().cloned())
            .chain(
                self.global
                    .training_runs
                    .values()
                    .map(|run| run.spec.room_id.clone()),
            )
            .collect();
        self.global
            .order_ids
            .as_ref()
            .unwrap()
            .fetch_max(self.global.next_order_id, Ordering::AcqRel);
        self.rooms.retain(|id, _| ids.contains(id));
        for id in ids {
            let app = self.global.extract_room(&id);
            let snapshot = PublishedRoom {
                rooms: Arc::new(app.rooms.clone()),
                dispatch: DispatchState::publish(&app, None, None),
            };
            let summary = RoomSummary::from_app(&app);
            let venues = app
                .rooms
                .simulation_room(&id)
                .ok()
                .map(|room| {
                    room.venue_ids()
                        .into_iter()
                        .map(|id| (id.to_owned(), Arc::new(AsyncMutex::new(()))))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(runtime) = self.rooms.get(&id) {
                *runtime
                    .state
                    .try_lock()
                    .expect("exclusive directory barrier") = Some(app);
                *runtime.published.write().unwrap() = snapshot;
                *runtime.summary.write().unwrap() = summary;
                *runtime.venues.write().unwrap() = venues;
            } else {
                self.rooms.insert(
                    id,
                    Arc::new(RoomRuntime {
                        state: Arc::new(AsyncMutex::new(Some(app))),
                        coordination: Arc::new(RwLock::new(())),
                        venues: std::sync::RwLock::new(venues),
                        published: std::sync::RwLock::new(snapshot),
                        summary: std::sync::RwLock::new(summary),
                    }),
                );
            }
        }
    }
}

impl AppState {
    fn read_context(&self) -> Self {
        Self {
            bot_registry: self.bot_registry.clone(),
            rooms: RoomManager::new(),
            executions: BTreeMap::new(),
            room_event_senders: BTreeMap::new(),
            next_order_id: self.next_order_id,
            order_ids: self.order_ids.clone(),
            base_url: self.base_url.clone(),
            agent_workers: BTreeMap::new(),
            schedulers: BTreeMap::new(),
            training_runs: BTreeMap::new(),
            platform: platform::PlatformData::default(),
            journal: self.journal.clone(),
            auth_policy: self.auth_policy.clone(),
            room_lease_runtime: None,
        }
    }

    fn extract_room(&mut self, id: &str) -> Self {
        let mut app = self.read_context();
        // Global metadata cannot change while room lanes hold the directory
        // reader. Refresh these references when the administrative barrier exits.
        app.platform = self.platform.clone();
        app.rooms = self.rooms.take_room(id).unwrap_or_default();
        if let Some(value) = self.executions.remove(id) {
            app.executions.insert(id.to_owned(), value);
        }
        if let Some(value) = self.room_event_senders.remove(id) {
            app.room_event_senders.insert(id.to_owned(), value);
        }
        if let Some(value) = self.agent_workers.remove(id) {
            app.agent_workers.insert(id.to_owned(), value);
        }
        if let Some(value) = self.schedulers.remove(id) {
            app.schedulers.insert(id.to_owned(), value);
        }
        let training: Vec<_> = self
            .training_runs
            .iter()
            .filter(|(_, run)| run.spec.room_id == id)
            .map(|(key, _)| key.clone())
            .collect();
        for key in training {
            app.training_runs
                .insert(key.clone(), self.training_runs.remove(&key).unwrap());
        }
        app.room_lease_runtime = self.room_lease_runtime.as_mut().map(|runtime| {
            let mut local = RoomLeaseRuntimeState {
                config: runtime.config.clone(),
                leases: BTreeMap::new(),
                lost_rooms: BTreeSet::new(),
                renew_failures: 0,
            };
            if let Some(lease) = runtime.leases.remove(id) {
                local.leases.insert(id.to_owned(), lease);
            }
            if runtime.lost_rooms.remove(id) {
                local.lost_rooms.insert(id.to_owned());
            }
            local
        });
        app
    }

    fn absorb_room(&mut self, mut app: Self) {
        self.rooms
            .join_disjoint(app.rooms)
            .expect("disjoint room ownership");
        self.executions.append(&mut app.executions);
        self.room_event_senders.append(&mut app.room_event_senders);
        self.agent_workers.append(&mut app.agent_workers);
        self.schedulers.append(&mut app.schedulers);
        self.training_runs.append(&mut app.training_runs);
        if let (Some(global), Some(mut local)) =
            (&mut self.room_lease_runtime, app.room_lease_runtime)
        {
            global.leases.append(&mut local.leases);
            global.lost_rooms.append(&mut local.lost_rooms);
            global.renew_failures = global.renew_failures.saturating_add(local.renew_failures);
        }
    }

    pub(super) fn reserve_order_ids(&self, count: u64) -> Result<OrderIdReservation, ApiError> {
        let counter = self
            .order_ids
            .as_ref()
            .expect("shared execution owner has an allocator")
            .clone();
        let mut start = counter.load(Ordering::Acquire);
        loop {
            let end = start
                .checked_add(count)
                .filter(|end| *end <= SYSTEM_LIQUIDATION_ORDER_ID_BASE)
                .ok_or_else(|| {
                    api_error(StatusCode::CONFLICT, "API order-id range is exhausted")
                })?;
            match counter.compare_exchange_weak(start, end, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(current) => start = current,
            }
        }
        Ok(OrderIdReservation {
            counter,
            start,
            end: start + count,
            used_end: start,
        })
    }
}

/// Allocate disjoint ranges with one atomic operation, never a global transaction
/// lock. Unused tails are returned if nobody allocated after us. Concurrent
/// transactions may leave gaps, but cannot reuse a committed order identity.
pub(super) struct OrderIdReservation {
    counter: Arc<AtomicU64>,
    pub(super) start: u64,
    end: u64,
    used_end: u64,
}
impl OrderIdReservation {
    pub(super) fn commit(&mut self, used_end: u64) {
        assert!(
            (self.start..=self.end).contains(&used_end),
            "order-id reservation exceeded"
        );
        self.used_end = used_end;
    }
}
impl Drop for OrderIdReservation {
    fn drop(&mut self) {
        let _ = self.counter.compare_exchange(
            self.end,
            self.used_end,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

impl Deref for StateGuard {
    type Target = AppState;
    fn deref(&self) -> &AppState {
        match self {
            Self::Global(guard) => &guard.directory.global,
            Self::Room(guard) => guard.state.as_ref().unwrap(),
        }
    }
}
impl DerefMut for StateGuard {
    fn deref_mut(&mut self) -> &mut AppState {
        match self {
            Self::Global(guard) => &mut guard.directory.global,
            Self::Room(guard) => {
                guard.changed = true;
                guard.market_changed = true;
                guard.state.as_mut().unwrap()
            }
        }
    }
}
impl StateGuard {
    /// Read the current immutable market under the same room lock as scheduler
    /// selection. Scheduler-only writes keep this identity; every market write
    /// publishes a new identity. Unpublished writes must use their own snapshot.
    #[cfg(test)]
    pub(super) fn market_snapshot(&self) -> Arc<RoomManager> {
        match self {
            Self::Room(guard) if !guard.market_changed => {
                guard.runtime.published.read().unwrap().rooms.clone()
            }
            _ => Arc::new(self.rooms.clone()),
        }
    }

    /// A scheduler-only commit cannot mutate the market snapshot. Keep summary
    /// publication, but don't replace a reader's identical immutable room tree.
    pub(super) fn scheduler_mut(
        &mut self,
        room: &str,
    ) -> Option<&mut exchange_core::SchedulerState> {
        match self {
            Self::Global(guard) => guard.directory.global.schedulers.get_mut(room),
            Self::Room(guard) => {
                guard.changed = true;
                guard.state.as_mut().unwrap().schedulers.get_mut(room)
            }
        }
    }

    /// The caller records the exact patch only after its durable installation.
    /// Unknown/full scheduler changes rebuild the derived view instead.
    pub(super) fn dispatch_patch_applied(&mut self, base: u64, indices: Vec<usize>) {
        if let Self::Room(guard) = self {
            guard.dispatch_patch = Some((base, indices));
        }
    }
}
impl Drop for GlobalGuard {
    fn drop(&mut self) {
        self.directory.partition();
    }
}
impl Drop for RoomGuard {
    fn drop(&mut self) {
        #[cfg(feature = "bench-execution-timing")]
        let _timer = crate::bench_execution_timing::Timer::start(
            &crate::bench_execution_timing::SCHEDULER_PUBLISH,
        );
        if self.changed {
            let app = self.state.as_ref().unwrap();
            debug_assert_eq!(
                app.platform.revision, self._directory.global.platform.revision,
                "platform updates must use the administrative barrier"
            );
            let summary = RoomSummary::from_app(app);
            let previous = self.runtime.published.read().unwrap().clone();
            let next = PublishedRoom {
                rooms: if self.market_changed {
                    Arc::new(app.rooms.clone())
                } else {
                    previous.rooms
                },
                dispatch: DispatchState::publish(
                    app,
                    previous.dispatch.as_ref(),
                    self.dispatch_patch.as_ref(),
                ),
            };
            let previous = std::mem::replace(&mut *self.runtime.published.write().unwrap(), next);
            drop(previous);
            *self.runtime.summary.write().unwrap() = summary;
        }
        #[cfg(feature = "bench-execution-timing")]
        bench_execution_timing::ROOM_HELD.record(self.timing_started);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn market_snapshot_identity_survives_scheduler_writes_but_not_market_writes() {
        let spec: exchange_core::population::BackgroundMarket = serde_json::from_str(include_str!(
            "../../scripts/fixtures/microstructure_market.json"
        ))
        .unwrap();
        let room = spec.scenario.room_id.clone();
        let mut app = AppState::new("http://localhost");
        app.rooms.create_room(spec.scenario).unwrap();
        app.schedulers.insert(
            room.clone(),
            exchange_core::SchedulerState::new(
                room.clone(),
                spec.agents,
                exchange_core::SchedulerMode::Manual,
            ),
        );
        let store = StateStore::new(app);
        let initial_dispatch = store.read_dispatch(&room).await.unwrap().dispatch.unwrap();
        let original = store.lock_room(&room).await.market_snapshot();
        {
            let mut guard = store.lock_room(&room).await;
            let scheduler = guard.scheduler_mut(&room).unwrap();
            let base = scheduler.revision;
            scheduler.revision += 1;
            scheduler.agents[0].unfinished_actions = vec![OrderAction::Cancel { order_id: 987 }];
            guard.dispatch_patch_applied(base, vec![0]);
            let pending = store.read_dispatch(&room).await.unwrap();
            assert!(Arc::ptr_eq(
                &initial_dispatch,
                pending.dispatch.as_ref().unwrap()
            ));
            assert!(Arc::ptr_eq(&original, &pending.rooms));
            assert!(Arc::ptr_eq(&original, &guard.market_snapshot()));
        }
        let updated = store.read_dispatch(&room).await.unwrap().dispatch.unwrap();
        assert!(Arc::ptr_eq(&initial_dispatch.roster, &updated.roster));
        assert_eq!(updated.revision, initial_dispatch.revision + 1);
        assert!(initial_dispatch.agents[0].unfinished_actions.is_empty());
        assert_eq!(
            updated.agents[0].unfinished_actions,
            vec![OrderAction::Cancel { order_id: 987 }]
        );
        assert_eq!(
            updated.agents.iter().skip(1).collect::<Vec<_>>(),
            initial_dispatch.agents.iter().skip(1).collect::<Vec<_>>()
        );
        assert!(
            updated
                .agents
                .iter()
                .zip(initial_dispatch.agents.iter())
                .skip(1)
                .all(|(after, before)| Arc::ptr_eq(after, before))
        );
        assert!(Arc::ptr_eq(
            &original,
            &store.lock_room(&room).await.market_snapshot()
        ));
        let old_step = original.clock(&room).unwrap().step();
        {
            let mut guard = store.lock_room(&room).await;
            guard.rooms.advance_clock(&room, 1).unwrap();
            let pending = guard.market_snapshot();
            assert!(!Arc::ptr_eq(&original, &pending));
            assert_eq!(pending.clock(&room).unwrap().step(), old_step + 1);
        }
        let committed = store.lock_room(&room).await.market_snapshot();
        assert!(!Arc::ptr_eq(&original, &committed));
        assert_eq!(committed.clock(&room).unwrap().step(), old_step + 1);
        assert_eq!(original.clock(&room).unwrap().step(), old_step);
    }

    #[tokio::test]
    async fn order_id_reservations_return_unused_tails_without_reusing_committed_ids() {
        let store = StateStore::new(AppState::new("http://localhost"));
        let app = store.metadata().await;
        let start = app.order_ids.as_ref().unwrap().load(Ordering::Acquire);
        {
            let _failed = app.reserve_order_ids(3).unwrap();
        }
        assert_eq!(
            app.order_ids.as_ref().unwrap().load(Ordering::Acquire),
            start
        );
        let first = app.reserve_order_ids(3).unwrap();
        let mut second = app.reserve_order_ids(4).unwrap();
        assert_eq!(second.start, first.start + 3);
        second.commit(second.start + 1);
        let committed = second.start;
        drop(second);
        drop(first);
        let next = app.reserve_order_ids(2).unwrap();
        assert_eq!(next.start, committed + 1);
    }

    #[tokio::test]
    async fn allocation_range_exhaustion_is_checked_before_advancing_the_counter() {
        let mut app = AppState::new("http://localhost");
        app.next_order_id = SYSTEM_LIQUIDATION_ORDER_ID_BASE - 1;
        let store = StateStore::new(app);
        let app = store.metadata().await;
        assert!(app.reserve_order_ids(2).is_err());
        let mut last = app.reserve_order_ids(1).unwrap();
        assert_eq!(last.start, SYSTEM_LIQUIDATION_ORDER_ID_BASE - 1);
        last.commit(SYSTEM_LIQUIDATION_ORDER_ID_BASE);
        drop(last);
        assert!(app.reserve_order_ids(1).is_err());
        assert!(app.reserve_order_ids(0).is_ok());
    }
}
