//! Opt-in causal observations. Writes a separate file; never changes a bot input.
use exchange_core::observation::ParticipantObservationView;
use exchange_core::*;
use serde_json::{Value, json};
use std::io::{BufWriter, Write};

pub struct Trace {
    writer: BufWriter<std::fs::File>,
    windows: Vec<(u64, u64)>,
}

pub fn focused(t: &AgentTemplate) -> bool {
    matches!(t, AgentTemplate::Plugin(p) if matches!(p.plugin_id.as_str(), "AdaptiveLiquidityMaker" | "BasisLinkTrader"))
}

pub fn state(bot: &dyn ScheduledBot) -> Value {
    let PersistedAgentKindState::Plugin { data, .. } = bot.snapshot() else {
        return Value::Null;
    };
    // Rolling history and identity are in the frozen recipe/final receipt. Keep
    // the actual scalar decision state here without repeating the whole window.
    let mut result = data.as_ref().clone();
    if let Some(map) = result.as_object_mut() {
        map.remove("identity");
        map.remove("born");
        map.remove("frames");
    }
    result
}

fn book(b: &BookSnapshot) -> Value {
    json!({"bids":b.bids.iter().take(8).collect::<Vec<_>>(),
        "asks":b.asks.iter().take(8).collect::<Vec<_>>()})
}

pub fn observation(v: &ParticipantObservationView<'_>) -> Value {
    let external = |levels: &[BookLevel], side: Side| {
        levels.iter().find_map(|l| {
            let mine: u64 = v
                .own_orders
                .iter()
                .filter(|o| o.side == side && o.price_tick == l.price_tick)
                .map(|o| o.remaining_qty)
                .sum();
            (l.qty > mine).then(|| json!({"price_tick":l.price_tick,"qty":l.qty-mine}))
        })
    };
    let external_levels = |levels: &[BookLevel], side: Side| {
        levels
            .iter()
            .filter_map(|l| {
                let mine: u64 = v
                    .own_orders
                    .iter()
                    .filter(|o| o.side == side && o.price_tick == l.price_tick)
                    .map(|o| o.remaining_qty)
                    .sum();
                (l.qty > mine).then(|| json!({"price_tick":l.price_tick,"qty":l.qty-mine}))
            })
            .collect::<Vec<_>>()
    };
    json!({"instrument":v.instrument_id,"time_ms":v.market_time_ms,"book":book(v.book),
        "external_bid":external(&v.book.bids,Side::Buy),"external_ask":external(&v.book.asks,Side::Sell),
        "external_bid_levels":external_levels(&v.book.bids,Side::Buy),"external_ask_levels":external_levels(&v.book.asks,Side::Sell),
        "account":v.own_account,"orders":v.own_orders,"public_trades":v.public_trades,
        "perp_price":v.perp_price,
        "closed_candles":v.bot_market_data.as_ref().map(|d| &d.candles),
        "related":v.related_markets.iter().map(observation).collect::<Vec<_>>()})
}

fn execution(e: &ActorExecution) -> Value {
    let result = match &e.result {
        ActorExecutionResult::Accepted(MarketExecution::Spot(r)) => {
            json!({"accepted":true,"command":r.command,"events":r.events,"clearing":r.clearing_events})
        }
        ActorExecutionResult::Accepted(MarketExecution::Perp(r)) => {
            json!({"accepted":true,"command":r.command,"events":r.events,"clearing":r.clearing_events})
        }
        ActorExecutionResult::Rejected(r) => json!({"accepted":false,"reason":format!("{r:?}")}),
    };
    json!({"time_ms":e.market_time_ms,"instrument":e.instrument_id,"command_seq":e.command_seq,
        "liquidation":e.is_liquidation,"result":result,"price_updates":e.price_updates,
        "funding":e.funding_settlement,"wallet_updates":e.wallet_updates,"margin_updates":e.margin_updates})
}

impl Trace {
    pub fn open() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let Some(path) = std::env::var_os("MF_RESEARCH_TRACE_PATH") else {
            return Ok(None);
        };
        let spec = std::env::var("MF_RESEARCH_TRACE_WINDOWS")?;
        let windows = spec
            .split(',')
            .map(|w| {
                let (a, b) = w.split_once(':').ok_or("expected start_ms:end_ms")?;
                let (a, b) = (a.parse::<u64>()?, b.parse::<u64>()?);
                if a >= b {
                    return Err("trace window must have positive length".into());
                }
                Ok((a, b))
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        let mut trace = Self {
            writer: BufWriter::new(
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?,
            ),
            windows,
        };
        trace.write(json!({"kind":"header","schema":1,"windows":trace.windows,
            "scope":"Exact pre-decision scalar state, account/orders, public tape/candles, eight book levels, and all visible own-order-excluded liquidity levels for makers/basis; ordered gateway actions and authoritative executions for all accounts. No matching, schedule, or input mutation."}))?;
        Ok(Some(trace))
    }
    pub fn active(&self, time: u64) -> bool {
        self.windows.iter().any(|(a, b)| *a <= time && time < *b)
    }
    pub fn write(&mut self, row: Value) -> std::io::Result<()> {
        serde_json::to_writer(&mut self.writer, &row)?;
        self.writer.write_all(b"\n")
    }
    pub fn decision(
        &mut self,
        t: &AgentTemplate,
        before: Value,
        view: Value,
        bot: &dyn ScheduledBot,
        actions: &[OrderAction],
    ) -> std::io::Result<()> {
        self.write(json!({"kind":"decision","participant":t.participant_id(),"account_id":t.config().account_id,
            "observation":view,"before":before,"after":state(bot),"actions":actions}))
    }
    pub fn account_book(
        rooms: &RoomManager,
        room: &str,
        instrument: &str,
        account: u64,
    ) -> Result<Value, Box<dyn std::error::Error>> {
        Ok(
            json!({"account":rooms.account_snapshot_for(room,instrument,account).map_err(|e|format!("{e:?}"))?,
            "book":book(&rooms.book_snapshot_for(room,instrument).map_err(|e|format!("{e:?}"))?)}),
        )
    }
    pub fn gateway(
        &mut self,
        t: &AgentTemplate,
        action: &OrderAction,
        e: &GatewayExecution,
        before: Value,
        after: Value,
        bot: &dyn ScheduledBot,
    ) -> std::io::Result<()> {
        self.write(json!({"kind":"gateway","participant":t.participant_id(),"account_id":t.config().account_id,
            "action":action,"execution":execution(&e.execution),"before":before,"after":after,
            "state_after_feedback":focused(t).then(||state(bot))}))
    }
    pub fn authoritative(&mut self, e: &ActorExecution) -> std::io::Result<()> {
        if self.active(e.market_time_ms) {
            self.write(json!({"kind":"authoritative","execution":execution(e)}))?;
        }
        Ok(())
    }
    pub fn finish(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}
