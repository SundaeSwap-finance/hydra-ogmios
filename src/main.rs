use anyhow::{Context, anyhow};
use pallas_primitives::conway::{Tx};
use serde_json::{Value};
use serde_json::json;
use std::net::UdpSocket;
use std::time::{SystemTime};

mod ogmios;
mod encode;
mod snapshot_confirmed;
mod utxo;

use crate::snapshot_confirmed::SnapshotConfirmed;
use crate::ogmios::{Block, Ogmios};
use crate::encode::{encode_json_tx};

use std::sync::{Arc, Mutex};
use std::net::TcpListener;
use std::thread::spawn;
use tungstenite::{Message, accept};

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
    HeadInitialized,
    CommittedUTxO,
    TickObserved,
    HeadOpened,
    TransactionAppliedToLocalUTxO,
    SnapshotRequested,
    PartySignedSnapshot,
    Unimplemented,
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
            "HeadInitialized" => Ok(StateChanged::HeadInitialized),
            "CommittedUTxO" => Ok(StateChanged::CommittedUTxO),
            "TickObserved" => Ok(StateChanged::TickObserved),
            "HeadOpened" => Ok(StateChanged::HeadOpened),
            "TransactionAppliedToLocalUTxO" => Ok(StateChanged::TransactionAppliedToLocalUTxO),
            "SnapshotRequested" => Ok(StateChanged::SnapshotRequested),
            "PartySignedSnapshot" => Ok(StateChanged::PartySignedSnapshot),
            _ => Ok(StateChanged::Unimplemented),
        }
    }
}

trait Source {
    fn read(&mut self) -> anyhow::Result<&[u8]>;
    fn done(&self) -> bool;
}

struct UdpSource {
    socket: UdpSocket,
    buf: [u8; 1048576]
}

impl UdpSource {
    fn new(socket: UdpSocket) -> UdpSource {
        UdpSource {
            socket: socket,
            buf: [0; 1048576],
        }
    }
}

impl Source for UdpSource {
    fn read(&mut self) -> anyhow::Result<&[u8]> {
        let (amt, _src) = self.socket.recv_from(&mut self.buf)?;
        let msg = &mut self.buf[..amt];
        Ok(msg)
    }
    fn done(&self) -> bool {
        false
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
    fn done(&self) -> bool {
        self.index >= self.contents.len()
    }
}

enum ChainsyncRequest {
    NextBlock,
}

impl TryFrom<Value> for ChainsyncRequest {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let jsonrpc = value["jsonrpc"].as_str().context("Invalid jsonrpc")?;
        let method = value["method"].as_str().context("Invalid method")?;
        if jsonrpc != "2.0" {
            return Err(anyhow!("unexpected jsonrpc: {}; expected \"2.0\"", jsonrpc));
        }
        if method != "nextBlock" {
            return Err(anyhow!("unexpected chainsync request: {}; expected \"nextBlock\"", method));
        }
        Ok(ChainsyncRequest::NextBlock)
    }
}

#[derive(Debug)]
struct NextBlockResponse {
    block: Block,
    tip: Block,
}

impl NextBlockResponse {
    fn new(block: Block, tip: Block) -> NextBlockResponse {
        NextBlockResponse{
            block: block,
            tip: tip
        }
    }
}

pub fn encode_next_block_response(x: &NextBlockResponse) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "nextBlock",
        "result": json!({
            "direction": "forward",
            "tip": json!({
                "slot": x.tip.time,
                "id": hex::encode(&x.tip.hash),
                "height": x.tip.height,
            }),
            "block": json!({
                "type": "praos",
                "era": "conway",
                "id": hex::encode(&x.block.hash),
                "slot": x.block.time,
                "height": x.block.height,
                "transactions": x.block.transactions.iter().map(|(hash, tx)| encode_json_tx(hash, &tx)).collect::<Vec<_>>(),
            }),
        }),
    })
}

fn listen<T: Source>(source: &mut T) -> anyhow::Result<()> {
    let ogmios = Arc::new(Mutex::new(Ogmios::new()));
    let ogmios_ref = Arc::clone(&ogmios);
    spawn(move || {
        let server = TcpListener::bind("127.0.0.1:9001").unwrap();
        for stream in server.incoming() {
            let ogmios = Arc::clone(&ogmios_ref);
            spawn(move || {
                let mut cursor = 0;
                let mut websocket = accept(stream.unwrap()).unwrap();
                loop {
                    let msg = websocket.read().unwrap();

                    // We do not want to send back ping/pong messages.
                    if msg.is_binary() || msg.is_text() {
                        let bytes = msg.into_data();
                        let v: Result<Value, _> = serde_json::from_slice(&bytes);
                        match v {
                            Ok(v) => {
                                let chainsync_request = ChainsyncRequest::try_from(v);
                                match chainsync_request {
                                    Ok(_req) => {
                                        loop {
                                            let ogmios = ogmios.lock().unwrap();
                                            if let Some((has_block, tip)) = ogmios.get_block(cursor) {
                                                let response = NextBlockResponse::new(has_block.clone(), tip.clone());
                                                let response_json = encode_next_block_response(&response);
                                                cursor += 1;
                                                websocket.send(Message::Text(response_json.to_string())).unwrap();
                                                break;
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        websocket.send(Message::Text(format!("couldn't parse request: {:?}", e))).unwrap()
                                    }
                                }
                            }
                            Err(e) => {
                                websocket.send(Message::Text(format!("couldn't parse request as json: {:?}", e))).unwrap()
                            }
                        }
                    }
                }
            });
        }
    });
    while !source.done() {
        let msg = source.read()?;
        let v: Value = serde_json::from_slice(&msg)?;
        let e: Event = Event::try_from(v.clone())?;
        let mut o = ogmios.lock().unwrap();
        match ogmios_consume_event(&mut o, e) {
            Ok(msg) => {
                println!("{:?}", msg);
            }
            Err(err) => {
                return Err(anyhow!("ogmios: error consuming hydra event: {}", err))
            }
        }
    }
    // TODO: wait on all websocket connections
    Ok(())
}

#[derive(Debug)]
enum OgmiosMessage {
    Message(String),
    NoMessage,
}

fn ogmios_consume_event(ogmios: &mut Ogmios, e: Event) -> anyhow::Result<OgmiosMessage> {
    match e.state_changed {
        StateChanged::TransactionReceived(transaction_received) => {
            let cbor_hex = transaction_received.tx.cbor_hex;
            let tx_cbor_raw = hex::decode(cbor_hex)?;
            let tx: Tx = minicbor::decode(&tx_cbor_raw[..])?;
            ogmios.add_transaction(&transaction_received.tx.tx_id, tx);
            Ok(OgmiosMessage::Message("added tx".to_string()))
        },
        StateChanged::SnapshotConfirmed(snapshot_confirmed) => {
            let mut tx_ids = vec![];
            for tx_id_raw in snapshot_confirmed.confirmed_transactions {
                let tx_id_hex = hex::encode(tx_id_raw);
                tx_ids.push(tx_id_hex);
            }
            let time = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
                Ok(n) => n.as_secs(),
                Err(_) => panic!("SystemTime before UNIX EPOCH!"),
            };
            match ogmios.new_block(&tx_ids, time) {
                Ok(()) => {
                    Ok(OgmiosMessage::Message("new block".to_string()))
                }
                Err(e) => {
                    Err(anyhow!("hydra-ogmios new_block failed: {}", e))
                }
            }
        },
        _ => {
           Ok(OgmiosMessage::Message("nothing to do".to_string()))
        },
    }
}

fn main() {
    let udp_socket = UdpSocket::bind("127.0.0.1:23457").expect("couldn't bind to udp");
    let mut source = UdpSource::new(udp_socket);
    listen(&mut source).expect("died")
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

    #[test]
    fn ogmios_handle_events() {
        let events = fs::read_to_string("testdata/events")
            .expect("couldn't read file");
        let lines = events.lines().map(|s| s.as_bytes().to_vec()).collect();
        let mut source = StaticSource{
            contents: lines,
            index: 0,
        };
        match listen(&mut source) {
            Ok(()) => {
            },
            Err(e) => {
                panic!("{}", e)
            },
        }
    }
}
