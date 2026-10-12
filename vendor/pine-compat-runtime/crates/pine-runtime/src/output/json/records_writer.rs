//! Allocation-free record encoding shared by full and incremental output.
use super::float_writer::float;
use super::*;
use std::io::{self, Write};

fn string<W: Write + ?Sized>(value: &str, output: &mut W) -> io::Result<()> {
    output.write_all(b"\"")?;
    value_writer::escaped(value, output)?;
    output.write_all(b"\"")
}
fn optional_string<W: Write + ?Sized>(value: Option<&str>, output: &mut W) -> io::Result<()> {
    match value {
        Some(value) => string(value, output),
        None => output.write_all(b"null"),
    }
}

pub(super) fn alerts<W: Write + ?Sized>(item: &AlertEvent, output: &mut W) -> io::Result<()> {
    write!(
        output,
        "{{\"id\":{},\"barIndex\":{},\"time\":{},\"message\":",
        item.id, item.bar_index, item.time
    )?;
    string(&item.message, output)?;
    output.write_all(b",\"source\":")?;
    string(&item.source, output)?;
    output.write_all(b"}")
}
pub(super) fn orders<W: Write + ?Sized>(
    item: &crate::StrategyOrderEvent,
    output: &mut W,
) -> io::Result<()> {
    output.write_all(b"{\"id\":")?;
    string(&item.id, output)?;
    write!(
        output,
        ",\"barIndex\":{},\"time\":{},\"direction\":",
        item.bar_index, item.time
    )?;
    string(&item.direction, output)?;
    output.write_all(b",\"qty\":")?;
    float(item.qty, output)?;
    output.write_all(b",\"price\":")?;
    float(item.price, output)?;
    output.write_all(b"}")
}
pub(super) fn order_fill_alerts<W: Write + ?Sized>(
    item: &crate::StrategyOrderFillAlertOutput,
    output: &mut W,
) -> io::Result<()> {
    output.write_all(b"{\"id\":")?;
    string(&item.id, output)?;
    write!(
        output,
        ",\"barIndex\":{},\"time\":{},\"direction\":",
        item.bar_index, item.time
    )?;
    string(&item.direction, output)?;
    output.write_all(b",\"qty\":")?;
    float(item.qty, output)?;
    output.write_all(b",\"price\":")?;
    float(item.price, output)?;
    output.write_all(b",\"entryId\":")?;
    optional_string(item.entry_id.as_deref(), output)?;
    output.write_all(b",\"exitId\":")?;
    optional_string(item.exit_id.as_deref(), output)?;
    output.write_all(b",\"message\":")?;
    string(&item.message, output)?;
    output.write_all(b"}")
}
pub(super) fn trades<W: Write + ?Sized>(
    item: &crate::StrategyTrade,
    output: &mut W,
) -> io::Result<()> {
    output.write_all(b"{\"id\":")?;
    string(&item.id, output)?;
    write!(
        output,
        ",\"entryBarIndex\":{},\"exitBarIndex\":{},\"entryTime\":{},\"exitTime\":{},\"entryPrice\":",
        item.entry_bar_index, item.exit_bar_index, item.entry_time, item.exit_time
    )?;
    float(item.entry_price, output)?;
    output.write_all(b",\"exitPrice\":")?;
    float(item.exit_price, output)?;
    output.write_all(b",\"qty\":")?;
    float(item.qty, output)?;
    output.write_all(b",\"profit\":")?;
    float(item.profit, output)?;
    output.write_all(b"}")
}
pub(super) fn position<W: Write + ?Sized>(
    item: &crate::StrategyPositionSnapshot,
    output: &mut W,
) -> io::Result<()> {
    write!(output, "{{\"barIndex\":{},\"size\":", item.bar_index)?;
    float(item.size, output)?;
    output.write_all(b",\"avgPrice\":")?;
    match item.avg_price {
        Some(value) => float(value, output)?,
        None => output.write_all(b"null")?,
    }
    output.write_all(b"}")
}
pub(super) fn equity<W: Write + ?Sized>(
    item: &crate::StrategyEquitySnapshot,
    output: &mut W,
) -> io::Result<()> {
    write!(output, "{{\"barIndex\":{},\"cash\":", item.bar_index)?;
    float(item.cash, output)?;
    output.write_all(b",\"marketValue\":")?;
    float(item.market_value, output)?;
    output.write_all(b",\"equity\":")?;
    float(item.equity, output)?;
    output.write_all(b",\"netProfit\":")?;
    float(item.net_profit, output)?;
    output.write_all(b"}")
}
pub(super) fn diagnostics<W: Write + ?Sized>(
    item: &crate::RuntimeDiagnostic,
    output: &mut W,
) -> io::Result<()> {
    output.write_all(b"{\"code\":")?;
    string(&item.code, output)?;
    output.write_all(b",\"message\":")?;
    string(&item.message, output)?;
    output.write_all(b"}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_encoding_preserves_field_order_escaping_nulls_and_negative_zero() {
        let alert = crate::StrategyOrderFillAlertOutput {
            id: "a\"\\\n汉🙂".into(),
            bar_index: 2,
            time: -3,
            direction: "long".into(),
            qty: -0.0,
            price: f64::INFINITY,
            entry_id: None,
            exit_id: Some("\0".into()),
            message: "\t".into(),
        };
        let mut bytes = Vec::new();
        order_fill_alerts(&alert, &mut bytes).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "{\"id\":\"a\\\"\\\\\\n汉🙂\",\"barIndex\":2,\"time\":-3,\"direction\":\"long\",\"qty\":-0,\"price\":null,\"entryId\":null,\"exitId\":\"\\u0000\",\"message\":\"\\t\"}"
        );
        for average in [None, Some(f64::NAN), Some(f64::NEG_INFINITY)] {
            let mut bytes = Vec::new();
            position(
                &crate::StrategyPositionSnapshot {
                    bar_index: 1,
                    size: -0.0,
                    avg_price: average,
                },
                &mut bytes,
            )
            .unwrap();
            assert_eq!(bytes, b"{\"barIndex\":1,\"size\":-0,\"avgPrice\":null}");
        }
    }
}
