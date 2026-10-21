/// Lets an order run on the live actor state instead of a staged copy, by
/// keeping what is needed to rebuild the state before it.
///
/// A staged copy shares every map node with the state it replaces, so each
/// order copies each node it writes and then frees the replaced ones. Instead
/// the actor keeps a checkpoint (a copy of itself) and the public mutations
/// applied since. An order runs in place; only when it must roll back is the
/// state before it rebuilt, from the checkpoint and a replay of the mutations
/// since, and the order run again staged, which gives the staged result.
/// Mutations are deterministic, so the replay reaches the same state. Every
/// public mutation either records itself or drops the checkpoint, and the
/// log is bounded, so a rebuild replays a bounded number of mutations.
#[derive(Clone, Default)]
struct UndoLog {
    checkpoint: Option<std::sync::Arc<ExchangeActor>>,
    ops: im::Vector<UndoOp>,
    /// Inside a public mutation or a replay: nested mutations are part of
    /// the outer one, so they neither record themselves nor run in place.
    busy: bool,
}

impl std::fmt::Debug for UndoLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UndoLog")
    }
}

/// A public mutation that is replayed to rebuild the state.
#[derive(Clone, Debug)]
enum UndoOp {
    Apply {
        instrument_id: String,
        command: Command,
        origin: CommandOrigin,
    },
    AdvanceClock(u64),
    AdvanceClockVenueOnly(u64),
    TakeClockExecutions,
    Liquidate {
        instrument_id: String,
        account_id: AccountId,
        order_id: OrderId,
    },
    CompleteFlatProtections,
    PreparePositionExits,
    RecordPositionExit {
        instrument_id: String,
        account_id: AccountId,
        side: crate::PositionSide,
        order_id: u64,
    },
    PrepareConditionalOrders,
    RecordConditionalOrder {
        order: crate::conditional_orders::ConditionalOrder,
        id: u64,
        accepted: bool,
    },
}

/// Mutations recorded before a new checkpoint is taken: a rebuild replays
/// at most this many.
#[cfg(not(test))]
const MAX_UNDO_OPS: usize = 256;
#[cfg(test)]
const MAX_UNDO_OPS: usize = 3;

#[cfg(test)]
thread_local! {
    /// Tests: orders run staged, as before the in-place path.
    pub(crate) static STAGED_ORDERS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Tests: every recorded mutation checks that the replay rebuilds the state.
    pub(crate) static VERIFY_UNDO: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    /// Tests that change actor fields directly: a mutation that finds the
    /// checkpoint no longer leads to the state drops it.
    pub(crate) static DIRECT_TEST_MUTATIONS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Tests: rebuilds, and those that replayed recorded mutations.
    pub(crate) static REBUILDS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

impl ExchangeActor {
    /// Starts a public mutation; true when it is the outermost one.
    fn begin_mutation(&mut self) -> bool {
        #[cfg(test)]
        if !self.undo.busy
            && DIRECT_TEST_MUTATIONS.get()
            && self.undo.checkpoint.is_some()
            && !self.replay_matches()
        {
            self.undo = UndoLog::default();
        }
        !std::mem::replace(&mut self.undo.busy, true)
    }

    /// Ends the outermost public mutation: records it, or (None) drops the
    /// checkpoint, which no longer leads to this state.
    fn end_mutation(&mut self, outer: bool, op: Option<UndoOp>) {
        if !outer {
            return;
        }
        self.undo.busy = false;
        match op {
            Some(op) if self.undo.checkpoint.is_some() => {
                self.undo.ops.push_back(op);
                #[cfg(test)]
                self.verify_undo();
                if self.undo.ops.len() >= MAX_UNDO_OPS {
                    self.undo = UndoLog::default();
                }
            }
            Some(_) => {}
            None => self.undo = UndoLog::default(),
        }
    }

    /// A public mutation that is not recorded.
    fn untracked<R>(&mut self, mutation: impl FnOnce(&mut Self) -> R) -> R {
        let outer = self.begin_mutation();
        let result = mutation(self);
        self.end_mutation(outer, None);
        result
    }

    /// A recorded public mutation.
    fn tracked<R>(
        &mut self,
        op: impl FnOnce() -> UndoOp,
        mutation: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let outer = self.begin_mutation();
        let result = mutation(self);
        self.end_mutation(outer, outer.then(op));
        result
    }

    /// Whether the outermost order may run in place: an ordinary order
    /// command, whose rollback the order transaction reports.
    fn order_runs_in_place(command: &Command) -> bool {
        #[cfg(test)]
        if STAGED_ORDERS.get() {
            return false;
        }
        !matches!(
            command,
            Command::SetConditionalOrder { .. }
                | Command::SetPositionProtection { .. }
                | Command::NewOrderWithProtection { .. }
        )
    }

    fn ensure_checkpoint(&mut self) {
        if self.undo.checkpoint.is_none() {
            let mut checkpoint = self.clone();
            checkpoint.undo = UndoLog::default();
            self.undo.checkpoint = Some(std::sync::Arc::new(checkpoint));
            self.undo.ops = im::Vector::new();
        }
    }

    /// The state the checkpoint and the recorded mutations lead to.
    fn replayed(&self) -> Self {
        let mut actor = (**self.undo.checkpoint.as_ref().expect("a checkpoint")).clone();
        actor.undo.busy = true;
        for op in self.undo.ops.iter().cloned() {
            actor.replay(op);
        }
        actor.undo = UndoLog::default();
        actor
    }

    /// Replaces the state, part-way through a failed in-place order, with the
    /// state before it; the outer mutation continues (and records nothing).
    fn rebuild_from_checkpoint(&mut self) {
        #[cfg(test)]
        {
            let (all, replaying) = REBUILDS.get();
            REBUILDS.set((all + 1, replaying + usize::from(!self.undo.ops.is_empty())));
        }
        *self = self.replayed();
        self.undo.busy = true;
    }

    fn replay(&mut self, op: UndoOp) {
        match op {
            UndoOp::Apply {
                instrument_id,
                command,
                origin,
            } => {
                let _ = self.apply_to_instrument_from(&instrument_id, command, origin);
            }
            UndoOp::AdvanceClock(steps) => {
                let _ = self.advance_clock(steps);
            }
            UndoOp::AdvanceClockVenueOnly(steps) => {
                let _ = self.advance_clock_venue_only(steps);
            }
            UndoOp::TakeClockExecutions => {
                self.take_clock_executions();
            }
            UndoOp::Liquidate {
                instrument_id,
                account_id,
                order_id,
            } => {
                let _ = self.liquidate_account(&instrument_id, account_id, order_id);
            }
            UndoOp::CompleteFlatProtections => self.complete_flat_position_protections(),
            UndoOp::PreparePositionExits => {
                self.prepare_position_exits();
            }
            UndoOp::RecordPositionExit {
                instrument_id,
                account_id,
                side,
                order_id,
            } => self.record_position_exit(&instrument_id, account_id, side, order_id),
            UndoOp::PrepareConditionalOrders => {
                self.prepare_conditional_orders();
            }
            UndoOp::RecordConditionalOrder {
                order,
                id,
                accepted,
            } => self.record_conditional_order(&order, id, accepted),
        }
    }

    /// Tests: the checkpoint and the recorded mutations rebuild this state
    /// (the replay leaves the tests' visit counters as they were).
    #[cfg(test)]
    fn replay_matches(&self) -> bool {
        let visits = (
            tests::SPOT_SCOPE_VISITS.get(),
            tests::VENUE_SCOPE_VISITS.get(),
        );
        let replayed = self.replayed();
        tests::SPOT_SCOPE_VISITS.set(visits.0);
        tests::VENUE_SCOPE_VISITS.set(visits.1);
        serde_json::to_string(&replayed).unwrap() == serde_json::to_string(self).unwrap()
            && replayed.pending_clock_executions == self.pending_clock_executions
    }

    #[cfg(test)]
    fn verify_undo(&self) {
        if VERIFY_UNDO.get() {
            assert!(self.replay_matches(), "the undo replay diverged");
        }
    }
}

/// The public mutations: each records itself (replayed by `replay`) or, if
/// rare, drops the checkpoint. Their bodies are the `_body` methods. A new
/// public mutation needs a wrapper here and a place in the test
/// `every_public_mutation_is_recorded_or_drops_the_checkpoint`.
impl ExchangeActor {
    pub fn apply_to_instrument_from(
        &mut self,
        instrument_id: &str,
        command: Command,
        origin: CommandOrigin,
    ) -> Result<ActorExecution, ActorRejectReason> {
        let outer = self.begin_mutation();
        let in_place = outer && Self::order_runs_in_place(&command);
        if in_place {
            self.ensure_checkpoint();
        }
        let op = (outer && self.undo.checkpoint.is_some()).then(|| UndoOp::Apply {
            instrument_id: instrument_id.to_string(),
            command: command.clone(),
            origin,
        });
        let result =
            match self.apply_to_instrument_from_mode(instrument_id, command, origin, in_place) {
                Ok(Some(execution)) => Ok(execution),
                Ok(None) => {
                    // The order must roll back: rebuild the state before it and
                    // run it staged, which rejects it as before.
                    self.rebuild_from_checkpoint();
                    let Some(UndoOp::Apply { command, .. }) = &op else {
                        unreachable!("an in-place order is recorded")
                    };
                    self.apply_to_instrument_from_mode(
                        instrument_id,
                        command.clone(),
                        origin,
                        false,
                    )
                    .map(|execution| execution.expect("a staged order never needs a rebuild"))
                }
                Err(reason) => Err(reason),
            };
        self.end_mutation(outer, op);
        result
    }

    pub fn advance_clock(&mut self, steps: u64) -> Result<Vec<VenueTransfer>, ClearingError> {
        self.tracked(
            || UndoOp::AdvanceClock(steps),
            |a| a.advance_clock_body(steps),
        )
    }

    pub fn advance_clock_venue_only(
        &mut self,
        steps: u64,
    ) -> Result<Vec<VenueTransfer>, ClearingError> {
        self.tracked(
            || UndoOp::AdvanceClockVenueOnly(steps),
            |a| a.advance_clock_venue_only_body(steps),
        )
    }

    pub(crate) fn take_clock_executions(&mut self) -> Vec<ActorExecution> {
        self.tracked(
            || UndoOp::TakeClockExecutions,
            Self::take_clock_executions_body,
        )
    }

    pub fn liquidate_account(
        &mut self,
        instrument_id: &str,
        account_id: AccountId,
        order_id: OrderId,
    ) -> Result<ActorExecution, ActorRejectReason> {
        self.tracked(
            || UndoOp::Liquidate {
                instrument_id: instrument_id.to_string(),
                account_id,
                order_id,
            },
            |a| a.liquidate_account_body(instrument_id, account_id, order_id),
        )
    }

    pub fn complete_flat_position_protections(&mut self) {
        self.tracked(
            || UndoOp::CompleteFlatProtections,
            Self::complete_flat_position_protections_body,
        )
    }

    pub fn prepare_position_exits(
        &mut self,
    ) -> Vec<crate::position_protection::PositionExitRequest> {
        self.tracked(
            || UndoOp::PreparePositionExits,
            Self::prepare_position_exits_body,
        )
    }

    pub fn record_position_exit(
        &mut self,
        instrument: &str,
        account: AccountId,
        side: crate::PositionSide,
        order_id: u64,
    ) {
        self.tracked(
            || UndoOp::RecordPositionExit {
                instrument_id: instrument.to_string(),
                account_id: account,
                side,
                order_id,
            },
            |a| a.record_position_exit_body(instrument, account, side, order_id),
        )
    }

    pub fn prepare_conditional_orders(
        &mut self,
    ) -> Vec<crate::conditional_orders::ConditionalOrder> {
        self.tracked(
            || UndoOp::PrepareConditionalOrders,
            Self::prepare_conditional_orders_body,
        )
    }

    pub fn record_conditional_order(
        &mut self,
        p: &crate::conditional_orders::ConditionalOrder,
        id: u64,
        accepted: bool,
    ) {
        self.tracked(
            || UndoOp::RecordConditionalOrder {
                order: p.clone(),
                id,
                accepted,
            },
            |a| a.record_conditional_order_body(p, id, accepted),
        )
    }

    pub fn set_portfolio_balance(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        total: Money,
    ) {
        self.untracked(|a| a.set_portfolio_balance_body(account_id, asset_id, total))
    }

    pub fn allocate_from_portfolio(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<(), VenueTransferRejectReason> {
        self.untracked(|a| a.allocate_from_portfolio_body(account_id, asset_id, amount))
    }

    pub fn apply_venue_asset_delta(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<(), VenueTransferRejectReason> {
        self.untracked(|a| a.apply_venue_asset_delta_body(account_id, asset_id, amount))
    }

    pub fn submit_deposit(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, ClearingError> {
        self.untracked(|a| a.submit_deposit_body(account_id, asset_id, amount))
    }

    pub fn submit_venue_deposit(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, ClearingError> {
        self.untracked(|a| a.submit_venue_deposit_body(account_id, asset_id, amount))
    }

    pub fn submit_rejected_venue_deposit(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
        reject_reason: VenueTransferRejectReason,
    ) -> VenueTransfer {
        self.untracked(|a| {
            a.submit_rejected_venue_deposit_body(account_id, asset_id, amount, reject_reason)
        })
    }

    pub fn submit_withdrawal(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, ClearingError> {
        self.untracked(|a| a.submit_withdrawal_body(account_id, asset_id, amount))
    }

    pub fn submit_venue_withdrawal(
        &mut self,
        account_id: AccountId,
        asset_id: impl Into<String>,
        amount: Money,
    ) -> Result<VenueTransfer, ClearingError> {
        self.untracked(|a| a.submit_venue_withdrawal_body(account_id, asset_id, amount))
    }

    pub(crate) fn normalize_after_restore(&mut self) -> Result<(), ClearingError> {
        self.untracked(Self::normalize_after_restore_body)
    }

    /// Changes made through the returned market are not recorded, so the
    /// checkpoint is dropped (unless inside a recorded mutation).
    pub fn primary_market_mut(&mut self) -> &mut MarketActor {
        self.untracked(|_| ());
        self.primary_market_mut_body()
    }

    /// As `primary_market_mut`.
    pub fn market_mut(
        &mut self,
        instrument_id: &str,
    ) -> Result<&mut MarketActor, ActorRejectReason> {
        self.untracked(|_| ());
        self.market_mut_body(instrument_id)
    }

    pub fn pause(&mut self) {
        self.untracked(Self::pause_body)
    }

    pub fn resume(&mut self) {
        self.untracked(Self::resume_body)
    }

    pub fn close(&mut self) {
        self.untracked(Self::close_body)
    }

    pub fn restore_status(&mut self, status: MarketStatus) {
        self.untracked(|a| a.restore_status_body(status))
    }

    pub fn create_account(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
    ) -> AccountSnapshot {
        self.untracked(|a| a.create_account_body(account_id, cash_balance))
    }

    pub fn create_spot_account_with_position(
        &mut self,
        account_id: AccountId,
        cash_balance: Money,
        position_qty: PositionQty,
    ) -> Result<SpotAccountSnapshot, ActorRejectReason> {
        self.untracked(|a| {
            a.create_spot_account_with_position_body(account_id, cash_balance, position_qty)
        })
    }

    #[cfg(test)]
    pub(crate) fn complete_flat_position_protections_reference(&mut self) {
        self.untracked(Self::complete_flat_position_protections_reference_body)
    }

    #[cfg(test)]
    pub(crate) fn prepare_position_exits_reference(
        &mut self,
    ) -> Vec<crate::position_protection::PositionExitRequest> {
        self.untracked(Self::prepare_position_exits_reference_body)
    }

    #[cfg(test)]
    pub(crate) fn prepare_conditional_orders_reference(
        &mut self,
    ) -> Vec<crate::conditional_orders::ConditionalOrder> {
        self.untracked(Self::prepare_conditional_orders_reference_body)
    }
}
