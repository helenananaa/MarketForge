//! Fixed-state view construction/serialization without HTTP or scheduling.
use exchange_core::*;
use serde::Serialize;
use std::{hint::black_box, path::PathBuf, time::Instant};

#[derive(Serialize)]
struct View {
    book: BookSnapshot,
    accounts: AccountSnapshots,
    perp_price: Option<PerpPriceSnapshot>,
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    assert_eq!(args.len(), 4, "state-json instrument iterations output");
    let room: SimulationRoom = serde_json::from_slice(&std::fs::read(&args[0]).unwrap()).unwrap();
    let room_id = room.room_id().to_string();
    let mut rooms = RoomManager::new();
    rooms.restore_simulation_room(room, Vec::new()).unwrap();
    let instrument = &args[1];
    let count: usize = args[2].parse().unwrap();
    let output = PathBuf::from(&args[3]);
    std::fs::create_dir_all(&output).unwrap();
    let view = || View {
        book: rooms.book_snapshot_for(&room_id, instrument).unwrap(),
        accounts: rooms.account_snapshots_for(&room_id, instrument).unwrap(),
        perp_price: rooms
            .simulation_room(&room_id)
            .unwrap()
            .perp_price_snapshot(instrument)
            .unwrap(),
    };
    for _ in 0..100 {
        black_box(serde_json::to_vec(&view()).unwrap());
    }
    let mut construct_ns = 0u128;
    let mut serialize_ns = 0u128;
    let mut bytes = 0;
    for _ in 0..count {
        let started = Instant::now();
        let value = black_box(view());
        construct_ns += started.elapsed().as_nanos();
        let started = Instant::now();
        let encoded = black_box(serde_json::to_vec(&value).unwrap());
        serialize_ns += started.elapsed().as_nanos();
        bytes = encoded.len();
    }
    std::fs::write(
        output.join("view.json"),
        serde_json::to_vec(&view()).unwrap(),
    )
    .unwrap();
    let result = serde_json::json!({"iterations":count,"response_bytes":bytes,
        "construct_ms":construct_ns as f64 / 1e6,"serialize_ms":serialize_ns as f64 / 1e6});
    std::fs::write(
        output.join("result.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    println!("{result}");
}
