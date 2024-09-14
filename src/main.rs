use anyhow::{Context, anyhow};
use pallas_primitives::conway::{Tx};
use serde_json::{Value};
use std::net::UdpSocket;

mod ogmios;
mod snapshot_confirmed;
mod utxo;

use crate::snapshot_confirmed::SnapshotConfirmed;
use crate::ogmios::Ogmios;

// src/Hydra/Events.hs 'StateEvent'
// This is the type sent to EventSinks.
#[allow(dead_code)]
#[derive(Debug)]
struct Event {
    event_id: u64,
    state_changed: StateChanged,
}

#[allow(dead_code)]
#[derive(Debug)]
struct TransactionReceived {
    tx: TransactionReceivedTx,
}

#[allow(dead_code)]
#[derive(Debug)]
struct TransactionReceivedTx {
    cbor_hex: String,
    description: String,
    tx_id: String,
    r#type: String,
}

#[allow(dead_code)]
#[derive(Debug)]
enum StateChanged {
    TransactionReceived(TransactionReceived),
    SnapshotConfirmed(SnapshotConfirmed),
}

impl TryFrom<Value> for Event {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let event_id = value["eventId"].as_u64().context("Invalid eventId")?;
        let state_changed = StateChanged::try_from(value["stateChanged"].clone())?;
        Ok(Event{
            event_id,
            state_changed,
        })
    }
}

impl TryFrom<Value> for TransactionReceived {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let tx = TransactionReceivedTx::try_from(value["tx"].clone())?;
        Ok(TransactionReceived{
            tx: tx,
        })
    }
}

impl TryFrom<Value> for TransactionReceivedTx {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let cbor_hex = value["cborHex"].as_str().context("Invalid cborHex")?;
        let description = value["description"].as_str().context("Invalid description")?;
        let tx_id = value["txId"].as_str().context("Invalid txId")?;
        let r#type = value["type"].as_str().context("Invalid type")?;
        Ok(TransactionReceivedTx{
            cbor_hex: cbor_hex.to_string(),
            description: description.to_string(),
            tx_id: tx_id.to_string(),
            r#type: r#type.to_string(),
        })
    }
}

impl TryFrom<Value> for StateChanged {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let tag = value["tag"].as_str().context("Invalid tag")?;

        match tag {
            "SnapshotConfirmed" => {
                SnapshotConfirmed::try_from(value).map(StateChanged::SnapshotConfirmed)
            }
            "TransactionReceived" => {
                TransactionReceived::try_from(value).map(StateChanged::TransactionReceived)
            }
            _ => Err(anyhow!("Unimplemented variant: {:?}", value)),
        }
    }
}

trait Source {
    fn read(&mut self) -> anyhow::Result<&[u8]>;
}

struct UdpSource {
    socket: UdpSocket,
    buf: [u8; 1048576]
}

impl Source for UdpSource {
    fn read(&mut self) -> anyhow::Result<&[u8]> {
        let (amt, src) = self.socket.recv_from(&mut self.buf)?;
        let msg = &mut self.buf[..amt];
        Ok(msg)
    }
}

struct StaticSource {
    contents: Vec<Vec<u8>>,
    index: usize,
}

impl Source for StaticSource {
    fn read(&mut self) -> anyhow::Result<&[u8]> {
        if self.index >= self.contents.len() {
            return Err(anyhow!("end of data"))
        }
        let result = &self.contents[self.index];
        self.index += 1;
        Ok(&result)
    }
}

fn listen<T: Source>(source: &mut T) -> anyhow::Result<()> {
    let mut ogmios = Ogmios::new();
    loop {
        let msg = source.read()?;
        let v: Value = serde_json::from_slice(&msg)?;
        let e: Event = Event::try_from(v.clone())?;
        match ogmios_consume_event(&mut ogmios, e) {
            Ok(()) => {
                println!("ogmios consumed hydra event");
            }
            Err(err) => {
                println!("ogmios: error consuming hydra event: {}", err);
            }
        }
    }
}

fn ogmios_consume_event(ogmios: &mut Ogmios, e: Event) -> anyhow::Result<()> {
    match e.state_changed {
        StateChanged::TransactionReceived(transaction_received) => {
            let cbor_hex = transaction_received.tx.cbor_hex;
            let tx_cbor_raw = hex::decode(cbor_hex)?;
            let tx: Tx = minicbor::decode(&tx_cbor_raw[..])?;
            ogmios.add_transaction(&transaction_received.tx.tx_id, tx);
        },
        StateChanged::SnapshotConfirmed(snapshot_confirmed) => {
            let mut tx_ids = vec![];
            for tx_id_raw in snapshot_confirmed.confirmed_transactions {
                let tx_id_hex = hex::encode(tx_id_raw);
                tx_ids.push(tx_id_hex);
            }
            match ogmios.new_block(&tx_ids) {
                Ok(new_txes) => {
                    println!("new txes: {:?}", new_txes)
                }
                Err(e) => {
                    return Err(anyhow!("hydra-ogmios new_block failed: {}", e))
                }
            }
        },
    }
    Ok(())
}

fn main() {
    println!("Hello, world!");
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::convert::{TryFrom};
    use hex;
    use serde_json::{Value};
    use std::fs;

    #[test]
    fn transaction_received_can_parse_json() {
        let transaction_received = fs::read_to_string("testdata/transaction_received.json")
            .expect("couldn't read file");
        let v: Value = serde_json::from_str(&transaction_received)
            .expect("couldn't parse event");
        let result: Event = Event::try_from(v)
            .expect("couldn't parse event");
        println!("Event: {:?}", result);
        match result.state_changed {
            StateChanged::TransactionReceived(tx) => {
                let tx_cbor_raw = hex::decode(tx.tx.cbor_hex).unwrap();
                let tx: Tx = minicbor::decode(&tx_cbor_raw[..])
                    .expect("couldn't decode cbor");
                println!("{:?}", tx);
            }
            _ => {
                panic!("unexpected event");
            }
        }
    }

    #[test]
    fn snapshot_confirmed_can_parse_json_as_event() {
        let snapshot_confirmed = fs::read_to_string("testdata/snapshot_confirmed.json")
            .expect("couldn't read file");
        let v: Value = serde_json::from_str(&snapshot_confirmed)
            .expect("couldn't parse string as json");
        let result: Event = Event::try_from(v)
            .expect("couldn't parse event");
        print!("SnapshotConfirmed: {:?}", result);
    }
}
