use anyhow::{Context, anyhow};
use std::collections::HashMap;
use pallas_traverse::ComputeHash;
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
use std::thread;
use std::thread::{ThreadId, spawn};
use tungstenite::http::Uri;
use tungstenite::{connect, ClientRequestBuilder, Message, accept};

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
    SubmitTransaction((String, String)),
}

impl TryFrom<Value> for ChainsyncRequest {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let jsonrpc = value["jsonrpc"].as_str().context("Invalid jsonrpc")?;
        let method = value["method"].as_str().context("Invalid method")?;
        if jsonrpc != "2.0" {
            return Err(anyhow!("unexpected jsonrpc: {}; expected \"2.0\"", jsonrpc));
        }
        if method == "nextBlock" {
            return Ok(ChainsyncRequest::NextBlock)
        } else if method == "submitTransaction" {
            let params = value["params"].as_object().context("Invalid params")?;
            let transaction = params["transaction"].as_object().context("Invalid transaction")?;
            let cbor = transaction["cbor"].as_str().context("Invalid cbor")?;
            let tx_cbor_raw = hex::decode(cbor)?;
            let tx: Tx = minicbor::decode(&tx_cbor_raw[..])?;
            let hash = tx.transaction_body.compute_hash();
            return Ok(ChainsyncRequest::SubmitTransaction((hex::encode(&hash), cbor.to_string())))
        } else {
            return Err(anyhow!("unexpected chainsync request: {}", method));
        }
    }
}

enum HydraWsMessage {
    TxValid(String),
    TxInvalid((String, String)),
    Unimplemented(String),
}

impl TryFrom<Value> for HydraWsMessage {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let tag = value["tag"].as_str().context("Invalid tag")?;
        if tag == "TxValid" {
            let transaction = value["transaction"].as_object().context("Invalid transaction")?;
            let tx_id = transaction["txId"].as_str().context("Invalid txId")?;
            return Ok(HydraWsMessage::TxValid(tx_id.to_string()));
        } else if tag == "TxInvalid" {
            let transaction = value["transaction"].as_object().context("Invalid transaction")?;
            let tx_id = transaction["txId"].as_str().context("Invalid txId")?;
            let validation_error = value["validationError"].as_object().context("Invalid validationError")?;
            let reason = validation_error["reason"].as_str().context("Invalid reason")?;
            return Ok(HydraWsMessage::TxInvalid((tx_id.to_string(), reason.to_string())));
        } else {
            return Ok(HydraWsMessage::Unimplemented(tag.to_string()));
        }
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
    // Hydra client thread, for submitting transactions from ogmios client to hydra node
    let uri: Uri = "ws://127.0.0.1:4001".parse().unwrap();
    let builder = ClientRequestBuilder::new(uri);
    let (mut socket, _) = connect(builder).unwrap();
    let hydra_client_queue = Arc::new(Mutex::new(Vec::<(ThreadId, String, String)>::new()));
    let hydra_client_send_queue = hydra_client_queue.clone();
    let hydra_submittx_mailbox = Arc::new(Mutex::new(HashMap::new()));
    let hydra_submittx_mailbox_1 = Arc::clone(&hydra_submittx_mailbox);
    spawn(move || {
        let mut tx_submit_tasks = HashMap::new();
        loop {
            let mut should_read = false;
            {
                let mut send = hydra_client_send_queue.lock().unwrap();
                if let Some((thread_id, tx_id, msg)) = send.pop() {
                    println!("got request for txsubmit to hydra via ogmios: {:?} {:?} {:?}", thread_id, tx_id, msg);
                    let message = Message::text(msg);
                    socket.send(message).unwrap();
                    should_read = true;
                    // We are awaiting a confirmation on this tx submission
                    tx_submit_tasks.insert(tx_id.to_string(), thread_id);
                }
            }
            // Read all responses
            if !should_read {
                continue;
            }
            while let Ok(msg) = socket.read() {
                if !should_read {
                    break;
                }
                println!("recieved hydra ws message: {:?}", msg);
                if msg.is_binary() || msg.is_text() {
                    let bytes = msg.into_data();
                    let v: Result<Value, _> = serde_json::from_slice(&bytes);
                    match v {
                        Ok(v) => {
                            println!("hydra ws message incoming: {:?}", v.to_string());
                            let hydra_ws_message = HydraWsMessage::try_from(v);
                            match hydra_ws_message {
                                Ok(HydraWsMessage::TxValid(txid)) => {
                                    // FIXME: we should probably have a vec of submit tasks per
                                    // thread id instead of just one so that an ogmios client can
                                    // do multiple tx submissions without waiting for us to respond
                                    if let Some(thread_id) = tx_submit_tasks.remove(&txid.to_string()) {
                                        println!("got txvalid: {:?}", txid);
                                        should_read = false;
                                        // Send back the response from hydra for this txid
                                        // submission
                                        let response = json!({
                                            "jsonrpc": "2.0",
                                            "method": "submitTransaction",
                                            "result": json!({
                                                "transaction": json!({
                                                    "id": txid,
                                                }),
                                            }),
                                        });
                                        let mut mailbox = hydra_submittx_mailbox_1.lock().unwrap();
                                        mailbox.insert(thread_id, response);
                                    }
                                }
                                Ok(HydraWsMessage::TxInvalid((txid, error))) => {
                                    if let Some(thread_id) = tx_submit_tasks.remove(&txid.to_string()) {
                                        println!("got txinvalid: {:?}", txid);
                                        should_read = false;
                                        let response = json!({
                                            "jsonrpc": "2.0",
                                            "method": "submitTransaction",
                                            "result": json!({
                                                "error": json!({
                                                    "code": json!(null),
                                                    "message": error,
                                                    "data": json!(null),
                                                }),
                                            }),
                                        });
                                        let mut mailbox = hydra_submittx_mailbox_1.lock().unwrap();
                                        mailbox.insert(thread_id, response);
                                    }
                                }
                                Ok(HydraWsMessage::Unimplemented(_)) => {
                                    // We don't care about any other hydra ws messages
                                }
                                Err(_) => {
                                    // We don't really care if we get some data we can't parse
                                }
                            }
                        },
                        Err(_) => {
                            // We don't care if the message doesn't even parse as json
                        }
                    }
                }
            }
        }
    });

    // Ogmios server
    let ogmios = Arc::new(Mutex::new(Ogmios::new()));
    let ogmios_ref = Arc::clone(&ogmios);
    let hydra_client_receive_queue = hydra_client_queue.clone();
    let hydra_submittx_mailbox_ogmios = Arc::clone(&hydra_submittx_mailbox);
    spawn(move || {
        let server = TcpListener::bind("127.0.0.1:9001").unwrap();
        for stream in server.incoming() {
            let ogmios = Arc::clone(&ogmios_ref);
            let hydra_client_receive_queue = Arc::clone(&hydra_client_receive_queue);
            let hydra_submittx_mailbox_ogmios = Arc::clone(&hydra_submittx_mailbox_ogmios);
            spawn(move || {
                let my_thread_id = thread::current().id();
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
                                    Ok(ChainsyncRequest::NextBlock) => {
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
                                    Ok(ChainsyncRequest::SubmitTransaction((tx_id, cbor_hex))) => {
                                        println!("got ogmios request for txsubmit: {:?} {:?} {:?}", my_thread_id, tx_id, cbor_hex);
                                        let submit_tx = json!({
                                            "tag": "NewTx",
                                            "transaction": json!({
                                                "cborHex": cbor_hex,
                                            }),
                                        });
                                        {
                                            let mut rcv = hydra_client_receive_queue.lock().unwrap();
                                            rcv.push((my_thread_id, tx_id, submit_tx.to_string()));
                                        }
                                        // Await response from hydra node
                                        println!("await response from hydra node comm thread");
                                        loop {
                                            let mut mailbox = hydra_submittx_mailbox_ogmios.lock().unwrap();
                                            if let Some(response) = mailbox.remove(&my_thread_id) {
                                                println!("got response from hydra node comm thread: {:?}", response);
                                                websocket.send(Message::Text(response.to_string())).unwrap();
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

    // Hydra udp sink reader
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
    fn parse_submit_transaction() {
        let tx2 = fs::read_to_string("testdata/tx_2")
            .expect("couldn't read file");
        let v: Value = serde_json::from_str(&tx2)
            .expect("couldn't parse tx as Value");
        let result: ChainsyncRequest = ChainsyncRequest::try_from(v)
            .expect("couldn't parse Value as ChainsyncRequest");
        if let ChainsyncRequest::SubmitTransaction((txid, _)) = result {
            assert_eq!(txid, "70289f45472d02e96ba160f9b3fa7144ef67c23bd1bcedb19c4fed8e48c806c5");
        } else {
            panic!("incorrect variant");
        }
    }

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
