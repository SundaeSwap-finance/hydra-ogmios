use anyhow::{Context, Result, anyhow};
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
use crate::ogmios::{FindIntersection, Block, Ogmios, Point, encode_point};
use crate::encode::{encode_json_tx};

use std::sync::Arc;
use futures::lock::Mutex;
use futures::stream::SplitSink;
use tokio::net::TcpListener;
use std::thread;
use std::thread::{ThreadId, spawn};
use tungstenite::http::Uri;
use tungstenite::{connect, ClientRequestBuilder, Message, accept};
use tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{WebSocketStream, connect_async};
use futures::SinkExt;
use futures::StreamExt;
use tokio::io::{AsyncReadExt};

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

enum ChainsyncRequest {
    NextBlock,
    SubmitTransaction((String, String)),
    FindIntersection(Vec<Point>),
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
        } else if method == "findIntersection" {
            let params = value["params"].as_object().context("Invalid params")?;
            let points = params["points"].as_array().context("Invalid points")?;
            let points = points.iter().map(|p| Point::try_from(p.clone()))
                .collect::<Result<Vec<Point>>>()?;
            return Ok(ChainsyncRequest::FindIntersection(points))
        } else {
            return Err(anyhow!("unexpected chainsync request: {}", method));
        }
    }
}

enum HydraWsMessage {
    TxValid((String, String)),
    TxInvalid((String, String)),
    SnapshotConfirmed(SnapshotConfirmed),
    TransactionReceived(TransactionReceived),
    Unimplemented(()),
}

impl TryFrom<Value> for HydraWsMessage {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let tag = value["tag"].as_str().context("Invalid tag")?;
        if tag == "TxValid" {
            let transaction = value["transaction"].as_object().context("Invalid transaction")?;
            let tx_id = transaction["txId"].as_str().context("Invalid txId")?;
            let tx_cbor = transaction["cborHex"].as_str().context("Invalid cborHex")?;
            return Ok(HydraWsMessage::TxValid((tx_id.to_string(), tx_cbor.to_string())));
        } else if tag == "TxInvalid" {
            let transaction = value["transaction"].as_object().context("Invalid transaction")?;
            let tx_id = transaction["txId"].as_str().context("Invalid txId")?;
            let validation_error = value["validationError"].as_object().context("Invalid validationError")?;
            let reason = validation_error["reason"].as_str().context("Invalid reason")?;
            return Ok(HydraWsMessage::TxInvalid((tx_id.to_string(), reason.to_string())));
        } else if tag == "SnapshotConfirmed" {
            return SnapshotConfirmed::try_from(value).map(HydraWsMessage::SnapshotConfirmed)
        } else if tag == "TransactionReceived" {
            return TransactionReceived::try_from(value).map(HydraWsMessage::TransactionReceived)
        } else {
            return Ok(HydraWsMessage::Unimplemented(()));
        }
    }
}

#[derive(Debug)]
enum FindIntersectionResponse {
    Found((Point, Point)),
    NotFound(Point),
}

impl FindIntersectionResponse {
    fn new(block: Point, tip: Point) -> FindIntersectionResponse {
        FindIntersectionResponse::Found((block, tip))
    }
}

fn encode_find_intersection_response(x: &FindIntersectionResponse) -> Value {
    match x {
        FindIntersectionResponse::Found((block, tip)) => {
            json!({
                "jsonrpc": "2.0",
                "method": "findIntersection",
                "result": json!({
                    "intersection": encode_point(&block),
                    "tip": encode_point(&tip),
                }),
            })
        }
        FindIntersectionResponse::NotFound(tip) => {
            json!({
                "jsonrpc": "2.0",
                "method": "findIntersection",
                "error": json!({
                    "code": 1000,
                    "message": "intersection not found",
                    "data": json!({
                        "tip": encode_point(&tip),
                    }),
                }),
            })
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

fn encode_next_block_response(x: &NextBlockResponse) -> Value {
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

enum ChainSyncEvent {
    NotifyNextBlock,
    RequestNextBlock,
    ReceivedTxStatus,
}

struct ChainSyncClient {
    cursor: usize,
    sender: SplitSink<WebSocketStream<tokio::net::TcpStream>, tungstenite::Message>,
    awaiting_next_block: u64,
}

async fn listen() -> anyhow::Result<()> {
    let ogmios = Arc::new(Mutex::new(Ogmios::new()));
    let sender_ogmios_ref = Arc::clone(&ogmios);
    let ogmios_ref_for_hydra_client = Arc::clone(&ogmios);

    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel::<ChainSyncEvent>();

    let send_mutex = Arc::new(Mutex::new(send));
    let send_mutex_1 = Arc::clone(&send_mutex);

    // Hydra client thread, for submitting transactions from ogmios client to hydra node
    let request = "ws://127.0.0.1:4001".into_client_request().unwrap();
    let (stream, _) = connect_async(request).await.unwrap();
    println!("websocket connected to hydra node");
    let (mut hydra_node_write, mut hydra_node_read) = stream.split();
    let hydra_client_queue = Arc::new(Mutex::new(Vec::<(ThreadId, String, String)>::new()));
    let hydra_submittx_mailbox = Arc::new(Mutex::new(HashMap::new()));
    let hydra_submittx_mailbox_1 = Arc::clone(&hydra_submittx_mailbox);
    let tx_submit_tasks = Arc::new(Mutex::new(HashMap::new()));
    let tx_submit_tasks_1 = Arc::clone(&tx_submit_tasks);
    let hydra_client_send_queue = hydra_client_queue.clone();
    tokio::spawn(async move {
        loop {
            {
                let mut send = hydra_client_send_queue.lock().await;
                if let Some((thread_id, tx_id, msg)) = send.pop() {
                    println!("got request for txsubmit to hydra via ogmios: {:?} {:?} {:?}", thread_id, tx_id, msg);
                    let message = Message::text(msg);
                    println!("sending message to the hydra node now");
                    hydra_node_write.send(message).await.unwrap();
                    // We are awaiting a confirmation on this tx submission
                    tx_submit_tasks_1.lock().await.insert(tx_id.to_string(), thread_id);
                }
            }
        }
    });
    let handle = tokio::spawn(async move {
        loop {
            println!("awaiting message from the hydra node");
            let msg = hydra_node_read.next().await.unwrap().unwrap();
            if msg.is_binary() || msg.is_text() {
                let bytes = msg.into_data();
                let v: Result<Value, _> = serde_json::from_slice(&bytes);
                match v {
                    Ok(v) => {
                        println!("hydra ws message incoming: {:?}", v.to_string());
                        let hydra_ws_message = HydraWsMessage::try_from(v);
                        match hydra_ws_message {
                            Ok(HydraWsMessage::TransactionReceived(transaction_received)) => {
                                let mut o = ogmios_ref_for_hydra_client.lock().await;
                                let event = Event {
                                    event_id: 0,
                                    state_changed: StateChanged::TransactionReceived(transaction_received),
                                };
                                match ogmios_consume_event(&mut o, event) {
                                    Ok(OgmiosMessage::Message(s)) => {
                                        println!("Ogmios state update: {:?}", s);
                                    }
                                    Ok(OgmiosMessage::NoMessage) => {
                                        //
                                    }
                                    Err(err) => {
                                        println!("ogmios: error consuming hydra event: {}", err)
                                    }
                                }
                            }
                            Ok(HydraWsMessage::SnapshotConfirmed(snapshot_confirmed)) => {
                                let mut o = ogmios_ref_for_hydra_client.lock().await;
                                let event = Event {
                                    event_id: 0,
                                    state_changed: StateChanged::SnapshotConfirmed(snapshot_confirmed),
                                };
                                match ogmios_consume_event(&mut o, event) {
                                    Ok(OgmiosMessage::Message(s)) => {
                                        println!("Ogmios state update: {:?}", s);
                                    }
                                    Ok(OgmiosMessage::NoMessage) => {
                                        //
                                    }
                                    Err(err) => {
                                        println!("ogmios: error consuming hydra event: {}", err)
                                    }
                                }
                                send_mutex_1.lock().await.send(ChainSyncEvent::NotifyNextBlock);
                            }
                            Ok(HydraWsMessage::TxValid((txid, txbody))) => {
                                let mut o = ogmios_ref_for_hydra_client.lock().await;
                                let event = Event {
                                    event_id: 0,
                                    state_changed: StateChanged::TransactionReceived(TransactionReceived {
                                        tx: TransactionReceivedTx {
                                            cbor_hex: txbody,
                                            tx_id: txid.clone(),
                                            description: "".to_string(),
                                            r#type: "".to_string(),
                                        },
                                    }),
                                };
                                match ogmios_consume_event(&mut o, event) {
                                    Ok(OgmiosMessage::Message(s)) => {
                                        println!("Ogmios state update: {:?}", s);
                                    }
                                    Ok(OgmiosMessage::NoMessage) => {
                                        //
                                    }
                                    Err(err) => {
                                        println!("ogmios: error consuming hydra event: {}", err)
                                    }
                                }
                                // FIXME: we should probably have a vec of submit tasks per
                                // thread id instead of just one so that an ogmios client can
                                // do multiple tx submissions without waiting for us to respond
                                if let Some(thread_id) = tx_submit_tasks.lock().await.remove(&txid.to_string()) {
                                    println!("got txvalid: {:?}", txid);
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
                                    let mut mailbox = hydra_submittx_mailbox_1.lock().await;
                                    mailbox.insert(thread_id, response);
                                    send_mutex_1.lock().await.send(ChainSyncEvent::ReceivedTxStatus);
                                }
                            }
                            Ok(HydraWsMessage::TxInvalid((txid, error))) => {
                                if let Some(thread_id) = tx_submit_tasks.lock().await.remove(&txid.to_string()) {
                                    println!("got txinvalid: {:?}", txid);
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
                                    let mut mailbox = hydra_submittx_mailbox_1.lock().await;
                                    mailbox.insert(thread_id, response);
                                    send_mutex_1.lock().await.send(ChainSyncEvent::ReceivedTxStatus);
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
    });

    // map of client send sinks
    let clients = Arc::new(Mutex::new(HashMap::<ThreadId, ChainSyncClient>::new()));

    let sender_clients_ref = Arc::clone(&clients);
    let sender_mailbox_ref = Arc::clone(&hydra_submittx_mailbox);
    // chainsync sender thread
    tokio::spawn(async move {
        loop {
            match receive.recv().await {
                None => {
                    // rx is closed
                }
                Some(ChainSyncEvent::NotifyNextBlock) => {
                }
                Some(ChainSyncEvent::RequestNextBlock) => {
                }
                Some(ChainSyncEvent::ReceivedTxStatus) => {
                }
            }
            let mut sender_clients = sender_clients_ref.lock().await;
            for (client_id, client) in sender_clients.iter_mut() {
                if let Some(response) = sender_mailbox_ref.lock().await.remove(&client_id) {
                    client.sender.send(Message::Text(response.to_string())).await;
                }
                if client.awaiting_next_block > 0 {
                    if let Some((has_block, tip)) = sender_ogmios_ref.lock().await.get_block(client.cursor) {
                        let response = NextBlockResponse::new(has_block.clone(), tip.clone());
                        let response_json = encode_next_block_response(&response);
                        client.cursor += 1;
                        client.awaiting_next_block -= 1;
                        client.sender.send(Message::Text(response_json.to_string())).await;
                    }
                }
            }
        }
    });

    let ogmios_server = TcpListener::bind("127.0.0.1:9001")
        .await
        .expect("Listening to TCP failed.");
    let ogmios_ref = Arc::clone(&ogmios);
    let hydra_client_receive_queue = hydra_client_queue.clone();
    let hydra_submittx_mailbox_ogmios = Arc::clone(&hydra_submittx_mailbox);
    while let Ok((stream, peer)) = ogmios_server.accept().await {
        let ogmios = Arc::clone(&ogmios_ref);
        let hydra_client_receive_queue = Arc::clone(&hydra_client_receive_queue);
        let hydra_submittx_mailbox_ogmios = Arc::clone(&hydra_submittx_mailbox_ogmios);
        let clients = Arc::clone(&clients);
        let send = Arc::clone(&send_mutex);
        tokio::spawn(async move {
            println!("New Connection : {}", peer);
            match tokio_tungstenite::accept_async(stream).await {
                Err(e) => println!("Websocket connection error : {}", e),
                Ok(ws_stream) => {
                    let my_thread_id = thread::current().id();
                    let (mut sender, mut receiver) = ws_stream.split();
                    clients.lock().await.insert(my_thread_id, ChainSyncClient {
                        sender: sender,
                        cursor: 0,
                        awaiting_next_block: 0,
                    });
                    loop {
                        let msg = receiver.next().await.unwrap().unwrap();

                        // We do not want to send back ping/pong messages.
                        if msg.is_binary() || msg.is_text() {
                            let bytes = msg.into_data();
                            let v: Result<Value, _> = serde_json::from_slice(&bytes);
                            match v {
                                Ok(v) => {
                                    let chainsync_request = ChainsyncRequest::try_from(v);
                                    match chainsync_request {
                                        Ok(ChainsyncRequest::NextBlock) => {
                                            if let Some(mut my_client) = clients.lock().await.get_mut(&my_thread_id) {
                                                my_client.awaiting_next_block += 1;
                                            }
                                            send.lock().await.send(ChainSyncEvent::RequestNextBlock);
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
                                                let mut rcv = hydra_client_receive_queue.lock().await;
                                                rcv.push((my_thread_id, tx_id, submit_tx.to_string()));
                                            }
                                        }
                                        Ok(ChainsyncRequest::FindIntersection(points)) => {
                                                let ogmios = ogmios.lock().await;
                                                match ogmios.find_intersection(&points) {
                                                    FindIntersection::Found((block, tip)) => {
                                                        let response = FindIntersectionResponse::new(block.clone(), tip.clone());
                                                        let response_json = encode_find_intersection_response(&response);
                                                        if let Point::Point(ps) = block {
                                                            if let Some(height) = ps.height {
                                                                if let Some(me) = clients.lock().await.get_mut(&my_thread_id) {
                                                                    me.cursor = height as usize;
                                                                }
                                                            }
                                                        }
                                                        if let Some(mut my_client) = clients.lock().await.get_mut(&my_thread_id) {
                                                            my_client.sender.send(Message::Text(response_json.to_string())).await;
                                                        }
                                                    }
                                                    FindIntersection::NotFound(tip) => {
                                                        let response = FindIntersectionResponse::NotFound(tip);
                                                        let response_json = encode_find_intersection_response(&response);
                                                        if let Some(mut my_client) = clients.lock().await.get_mut(&my_thread_id) {
                                                            my_client.sender.send(Message::Text(response_json.to_string())).await;
                                                        }
                                                    }
                                                }
                                        }
                                        Err(e) => {
                                            //sender.send(Message::Text(format!("couldn't parse request: {:?}", e))).await;
                                        }
                                    }
                                }
                                Err(e) => {
                                    //sender.send(Message::Text(format!("couldn't parse request as json: {:?}", e))).await;
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    let () = handle.await.unwrap();
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
           Ok(OgmiosMessage::NoMessage)
        },
    }
}

#[tokio::main]
async fn main() {
    listen().await.unwrap();
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
    fn can_parse_find_intersect() {
        let find_intersection = fs::read_to_string("testdata/find_intersection.json")
            .expect("couldn't read file");
        let v: Value = serde_json::from_str(&find_intersection)
            .expect("couldn't parse string as json");
        let _result: ChainsyncRequest = ChainsyncRequest::try_from(v)
            .expect("couldn't parse chainsync request");
    }
}
