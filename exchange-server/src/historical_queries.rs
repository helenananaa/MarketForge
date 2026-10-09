use exchange_core::candles::Candle;
use postgres::Client;

use crate::journal::JournalError;

/// Prices stay integers. Equal-time open/close ordering follows the journal
/// command/event cursor rather than an ambiguous timestamp-only argMin/argMax.
pub(crate) fn candles(
    client: &mut Client,
    user_id: &str,
    room_id: &str,
    instrument_id: &str,
    interval_ms: u64,
    now_ms: u64,
    after_open_time_ms: Option<u64>,
) -> Result<Option<Vec<Candle>>, JournalError> {
    let mut tx = client
        .build_transaction()
        .isolation_level(postgres::IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .map_err(JournalError::Postgres)?;
    // Older journal schemas lack authoritative simulation timestamps. Do not
    // invent market times from wall-clock created_at; use full replay instead.
    let missing: bool = tx.query_one(
        "SELECT EXISTS(SELECT 1 FROM marketforge_market_ticks WHERE room_id=$1 AND instrument_id=$2 AND market_time_ms IS NULL) AND EXISTS(SELECT 1 FROM marketforge_room_members WHERE room_id=$1 AND user_id=$3)",
        &[&room_id, &instrument_id, &user_id],
    ).map_err(JournalError::Postgres)?.get(0);
    if missing {
        tx.rollback().map_err(JournalError::Postgres)?;
        let recovery = crate::journal::load_postgres_recovery(client, Some(room_id), false)?;
        let rooms = crate::recover_rooms_for_full_replay(&recovery)?;
        let mut trades = rooms
            .timed_trades(room_id, instrument_id)
            .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
        trades.retain(|trade| trade.market_time_ms <= now_ms);
        let mut candles = exchange_core::candles::aggregate_candles(&trades, interval_ms, now_ms)
            .map_err(|error| JournalError::Recovery(format!("{error:?}")))?;
        if let Some(after) = after_open_time_ms {
            candles.retain(|candle| candle.open_time_ms > after);
        }
        return Ok(Some(candles));
    }
    let interval = interval_ms.to_string();
    let now = now_ms.to_string();
    let after = after_open_time_ms.map(|value| value.to_string());
    let rows = tx.query(
        r#"
        WITH ticks AS (
          SELECT price_tick,qty,taker_side,market_time_ms,command_seq,event_seq,
                 floor(market_time_ms::numeric / $4::text::numeric) * $4::text::numeric AS bucket
          FROM marketforge_market_ticks
          WHERE room_id=$1 AND instrument_id=$2
            AND market_time_ms::numeric <= $5::text::numeric
            AND ($6::text IS NULL OR market_time_ms::numeric >=
                 (floor($6::text::numeric / $4::text::numeric)+1)*$4::text::numeric)
            AND EXISTS (SELECT 1 FROM marketforge_room_members WHERE room_id=$1 AND user_id=$3)
        ), ranked AS (
          SELECT *,row_number() OVER(PARTITION BY bucket ORDER BY market_time_ms,command_seq,event_seq) AS first_row,
                   row_number() OVER(PARTITION BY bucket ORDER BY market_time_ms DESC,command_seq DESC,event_seq DESC) AS last_row
          FROM ticks
        )
        SELECT bucket::text AS bucket,
          max(price_tick) FILTER (WHERE first_row=1) AS open,
          max(price_tick) AS high,min(price_tick) AS low,
          max(price_tick) FILTER (WHERE last_row=1) AS close,
          least(sum(qty)::numeric,18446744073709551615)::text AS volume,
          sum(price_tick::numeric * qty::numeric)::text AS quote_volume,
          count(*)::text AS trades,
          least(coalesce(sum(qty) FILTER (WHERE taker_side='buy'),0)::numeric,18446744073709551615)::text AS taker_buy_base,
          coalesce(sum(price_tick::numeric * qty::numeric) FILTER (WHERE taker_side='buy'),0)::text AS taker_buy_quote
        FROM ranked GROUP BY ranked.bucket ORDER BY ranked.bucket
        "#,
        &[&room_id, &instrument_id, &user_id, &interval, &now, &after],
    ).map_err(JournalError::Postgres)?;
    let candles = rows
        .into_iter()
        .map(|row| {
            let parse = |field: &str| -> Result<u64, JournalError> {
                row.get::<_, String>(field)
                    .parse()
                    .map_err(|_| JournalError::Recovery(format!("invalid candle {field}")))
            };
            let open_time_ms = parse("bucket")?;
            let close_time_ms = open_time_ms.saturating_add(interval_ms);
            Ok(Candle {
                schema_version: exchange_core::candles::CANDLE_SCHEMA_VERSION,
                open_time_ms,
                close_time_ms,
                open_tick: row.get("open"),
                high_tick: row.get("high"),
                low_tick: row.get("low"),
                close_tick: row.get("close"),
                volume: parse("volume")?,
                quote_volume: row.get::<_, String>("quote_volume").parse().map_err(|_| {
                    JournalError::Recovery("candle quote volume overflow".to_string())
                })?,
                trades: parse("trades")?,
                taker_buy_base: Some(parse("taker_buy_base")?),
                taker_buy_quote: Some(row.get::<_, String>("taker_buy_quote").parse().map_err(
                    |_| JournalError::Recovery("candle taker quote overflow".to_string()),
                )?),
                is_final: now_ms >= close_time_ms,
            })
        })
        .collect::<Result<Vec<_>, JournalError>>()?;
    tx.commit().map_err(JournalError::Postgres)?;
    Ok(Some(candles))
}
