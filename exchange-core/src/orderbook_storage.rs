//! Keep the checkpoint contract independent of the live indexed-node layout.
//! Serialization streams legacy price queues; recovery rebuilds all links,
//! aggregate quantities and account indexes from those queues.
use super::*;
use serde::{
    Deserializer, Serializer,
    de::Error,
    ser::{SerializeMap, SerializeSeq, SerializeStruct},
};
use std::collections::{BTreeMap, VecDeque};

impl Serialize for OrderBook {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut wire = serializer.serialize_struct("OrderBook", 8)?;
        wire.serialize_field("reduce_orders", &self.reduce_orders)?;
        wire.serialize_field("order_expirations", &self.order_expirations)?;
        wire.serialize_field("bids", &SideWire(self, Side::Buy))?;
        wire.serialize_field("asks", &SideWire(self, Side::Sell))?;
        wire.serialize_field("order_index", &IndexWire(self))?;
        wire.serialize_field("seen_order_ids", &self.seen_order_ids)?;
        wire.serialize_field("next_seq", &self.next_seq)?;
        wire.serialize_field("next_trade_id", &self.next_trade_id)?;
        wire.end()
    }
}

struct SideWire<'a>(&'a OrderBook, Side);
impl Serialize for SideWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let side = self.0.book_side(self.1);
        let mut map = serializer.serialize_map(Some(side.len()))?;
        for (price, level) in side {
            map.serialize_entry(price, &LevelWire(self.0, level))?;
        }
        map.end()
    }
}

struct LevelWire<'a>(&'a OrderBook, &'a PriceLevel);
impl Serialize for LevelWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.1.len))?;
        for order in self.0.level_orders(self.1) {
            seq.serialize_element(order)?;
        }
        seq.end()
    }
}

struct IndexWire<'a>(&'a OrderBook);
impl Serialize for IndexWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.order_index.len()))?;
        for (id, node) in self.0.order_index.iter() {
            map.serialize_entry(id, &node.location())?;
        }
        map.end()
    }
}

#[derive(Deserialize)]
struct BookWire {
    #[serde(default)]
    reduce_orders: OrdSet<OrderId>,
    #[serde(default)]
    order_expirations: OrdMap<OrderId, u64>,
    bids: BTreeMap<PriceTick, VecDeque<Order>>,
    asks: BTreeMap<PriceTick, VecDeque<Order>>,
    // Old indexes are derived data. Decode them for schema compatibility,
    // but never trust them as links into the restored book.
    #[serde(default, rename = "order_index")]
    _order_index: std::collections::HashMap<OrderId, OrderLocation>,
    seen_order_ids: OrdSet<OrderId>,
    next_seq: u64,
    next_trade_id: u64,
}

impl<'de> Deserialize<'de> for OrderBook {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = BookWire::deserialize(deserializer)?;
        let mut book = OrderBook {
            reduce_orders: wire.reduce_orders,
            order_expirations: wire.order_expirations,
            seen_order_ids: wire.seen_order_ids,
            next_seq: wire.next_seq,
            next_trade_id: wire.next_trade_id,
            ..OrderBook::default()
        };
        for (side, levels) in [(Side::Buy, wire.bids), (Side::Sell, wire.asks)] {
            for (price, queue) in levels {
                for order in queue {
                    if order.side != side
                        || order.price_tick != price
                        || order.remaining_qty == 0
                        || book.order_index.contains_key(&order.order_id)
                    {
                        return Err(D::Error::custom(
                            "inconsistent or duplicate resting order in checkpoint",
                        ));
                    }
                    book.insert_resting_order(order);
                }
            }
        }
        Ok(book)
    }
}
