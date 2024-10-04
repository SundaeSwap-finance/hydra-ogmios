use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use pallas_traverse::ComputeHash;
use pallas_primitives::conway::{Tx};
use serde_json::{Value};
use serde_json::json;
use tokio::net::UdpSocket;
use std::time::{SystemTime};
use clap::Parser;

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
use futures::stream::SplitStream;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use std::thread;
use std::thread::{ThreadId, spawn};
use tungstenite::http::Uri;
use tungstenite::{connect, ClientRequestBuilder, Message, accept};
use tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use futures::SinkExt;
use futures::StreamExt;
use tokio::io::{AsyncReadExt};
use tokio::task::{JoinHandle};

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
    RespondClient((ThreadId, Value)),
}

struct ChainSyncClient {
    cursor: usize,
    sender: SplitSink<WebSocketStream<TcpStream>, tungstenite::Message>,
    awaiting_next_block: u64,
}

struct Config {
    udp_address: String,
    hydra_node_client_address: String,
    ogmios_server_address: String,
}

fn submit_transaction_success(txid: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "submitTransaction",
        "result": json!({
            "transaction": json!({
                "id": txid,
            }),
        }),
    })
}

fn submit_transaction_error(error: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "submitTransaction",
        "result": json!({
            "error": json!({
                "code": json!(null),
                "message": error,
                "data": json!(null),
            }),
        }),
    })
}

type HydraStream = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;
type ChainSyncEventChannel = Arc<Mutex<UnboundedSender<ChainSyncEvent>>>;
type SubmitTasks = Arc<Mutex<HashMap<String, ThreadId>>>;

fn listen_hydra_udp(
    udp_address: String,
    send: ChainSyncEventChannel,
    pending_submissions: SubmitTasks,
    ogmios: Arc<Mutex<Ogmios>>) -> JoinHandle<()>
{
    tokio::spawn(async move {
        let sock = UdpSocket::bind(udp_address).await.unwrap();
        let mut buf = [0; 1048576];
        loop {
            let n = sock.recv(&mut buf).await.unwrap();
            let msg = &buf[..n];
            let v: Result<Value, _> = serde_json::from_slice(&msg);
            match v {
                Ok(v) => {
                    let event = Event::try_from(v);
                    match event {
                        Ok(e) => {
                            match e.state_changed {
                                StateChanged::TransactionReceived(_) => {
                                    let mut o = ogmios.lock().await;
                                    let _ = ogmios_consume_event(&mut o, e);
                                }
                                StateChanged::SnapshotConfirmed(_) => {
                                    let mut o = ogmios.lock().await;
                                    let _ = ogmios_consume_event(&mut o, e);
                                    send.lock().await.send(ChainSyncEvent::NotifyNextBlock);
                                }
                                _ => {
                                }
                            }
                        }
                        Err(_) => {
                        }
                    }
                }
                Err(_) => {
                }
            }
        }
    })
}

fn read_hydra_node_websocket(
    mut hydra_node_read: HydraStream,
    send_mutex_1: ChainSyncEventChannel,
    pending_submissions: SubmitTasks,
    ogmios: Arc<Mutex<Ogmios>>) -> JoinHandle<()>
{
    // This thread reads messages from the hydra node's websocket API
    tokio::spawn(async move {
        loop {
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
                                let mut o = ogmios.lock().await;
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
                                let mut o = ogmios.lock().await;
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
                                let mut o = ogmios.lock().await;
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
                                if let Some(thread_id) = pending_submissions.lock().await.remove(&txid.to_string()) {
                                    println!("got txvalid: {:?}", txid);
                                    let response = submit_transaction_success(txid);
                                    send_mutex_1.lock().await.send(ChainSyncEvent::RespondClient((thread_id, response)));
                                }
                            }
                            Ok(HydraWsMessage::TxInvalid((txid, error))) => {
                                if let Some(thread_id) = pending_submissions.lock().await.remove(&txid.to_string()) {
                                    println!("got txinvalid: {:?}", txid);
                                    let response = submit_transaction_error(error);
                                    send_mutex_1.lock().await.send(ChainSyncEvent::RespondClient((thread_id, response)));
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
    })
}

type ClientState = (ThreadId, String, String);

async fn start_ogmios_server(
    ogmios_server_address: String,
    ogmios: Arc<Mutex<Ogmios>>,
    hydra_client_tx: Arc<Mutex<UnboundedSender<ClientState>>>,
    send: Arc<Mutex<UnboundedSender<ChainSyncEvent>>>,
    clients: Arc<Mutex<HashMap<ThreadId, ChainSyncClient>>>
) {
    let ogmios_server = TcpListener::bind(ogmios_server_address)
        .await
        .expect("Listening to TCP failed.");
    while let Ok((stream, peer)) = ogmios_server.accept().await {
        let ogmios = Arc::clone(&ogmios);
        let hydra_client_tx = Arc::clone(&hydra_client_tx);
        let clients = Arc::clone(&clients);
        let send = Arc::clone(&send);
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
                        let msg_maybe = receiver.next().await.unwrap();
                        if let Err(e) = msg_maybe {
                            println!("disconnected");
                            return;
                        }
                        let msg = msg_maybe.unwrap();

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
                                            hydra_client_tx.lock().await.send((my_thread_id, tx_id, submit_tx.to_string()));
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
                                                            let _ = my_client.sender.send(Message::Text(response_json.to_string())).await;
                                                        }
                                                    }
                                                    FindIntersection::NotFound(tip) => {
                                                        let response = FindIntersectionResponse::NotFound(tip);
                                                        let response_json = encode_find_intersection_response(&response);
                                                        if let Some(mut my_client) = clients.lock().await.get_mut(&my_thread_id) {
                                                            let _ = my_client.sender.send(Message::Text(response_json.to_string())).await;
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

}

fn output_to_ogmios_clients(
    mut receive: UnboundedReceiver<ChainSyncEvent>,
    clients: Arc<Mutex<HashMap<ThreadId, ChainSyncClient>>>,
    ogmios: Arc<Mutex<Ogmios>>
) {
    // This thread waits for events and serves incoming requests from the chainsync client
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
                Some(ChainSyncEvent::RespondClient((thread_id, response))) => {
                    let mut clients = clients.lock().await;
                    match clients.get_mut(&thread_id) {
                        Some(client) => {
                            client.sender.send(Message::Text(response.to_string())).await;
                        }
                        None => {
                            // client doesn't exist
                        }
                    }
                }
            }
            let mut clients = clients.lock().await;
            for (client_id, client) in clients.iter_mut() {
                if client.awaiting_next_block > 0 {
                    if let Some((has_block, tip)) = ogmios.lock().await.get_block(client.cursor) {
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
}

async fn listen_websocket(config: Config) -> anyhow::Result<()> {
    let ogmios = Arc::new(Mutex::new(Ogmios::new()));

    let (send, mut receive) = mpsc::unbounded_channel::<ChainSyncEvent>();
    let send = Arc::new(Mutex::new(send));

    let request = config.hydra_node_client_address.into_client_request().unwrap();
    let (stream, _) = connect_async(request).await.unwrap();
    let (mut hydra_node_write, mut hydra_node_read) = stream.split();
    println!("websocket connected to hydra node");

    let (mut hydra_client_tx, mut hydra_client_rx) = mpsc::unbounded_channel::<ClientState>();

    let pending_submissions = Arc::new(Mutex::new(HashMap::new()));
    let pending_submissions_1 = Arc::clone(&pending_submissions);

    // This thread sends messages to the hydra node's websocket API
    tokio::spawn(async move {
        loop {
            {
                if let Some((thread_id, tx_id, msg)) = hydra_client_rx.recv().await {
                    println!("got request for txsubmit to hydra via ogmios: {:?} {:?} {:?}", thread_id, tx_id, msg);
                    let message = Message::text(msg);
                    hydra_node_write.send(message).await.unwrap();
                    // We are awaiting a confirmation on this tx submission
                    pending_submissions_1.lock().await.insert(tx_id.to_string(), thread_id);
                }
            }
        }
    });

    let handle = read_hydra_node_websocket(
        hydra_node_read,
        Arc::clone(&send),
        pending_submissions,
        Arc::clone(&ogmios)
    );

    // map of client send sinks
    let clients = Arc::new(Mutex::new(HashMap::<ThreadId, ChainSyncClient>::new()));

    output_to_ogmios_clients(
        receive,
        Arc::clone(&clients),
        Arc::clone(&ogmios)
    );

    start_ogmios_server(
        config.ogmios_server_address,
        Arc::clone(&ogmios),
        Arc::new(Mutex::new(hydra_client_tx)),
        Arc::clone(&send),
        Arc::clone(&clients),
    ).await;

    let () = handle.await.unwrap();
    Ok(())
}

async fn listen_udp(config: Config) -> anyhow::Result<()> {
    let ogmios = Arc::new(Mutex::new(Ogmios::new()));

    let (send, mut receive) = mpsc::unbounded_channel::<ChainSyncEvent>();
    let send = Arc::new(Mutex::new(send));


    let (mut hydra_client_tx, mut hydra_client_rx) = mpsc::unbounded_channel::<ClientState>();

    let pending_submissions = Arc::new(Mutex::new(HashMap::new()));
    let pending_submissions_1 = Arc::clone(&pending_submissions);

    let hydra_node_client_address = config.hydra_node_client_address;
    // This thread sends messages to the hydra node's websocket API
    tokio::spawn(async move {
        loop {
            {
                if let Some((thread_id, tx_id, msg)) = hydra_client_rx.recv().await {
                    println!("got request for txsubmit to hydra via ogmios: {:?} {:?} {:?}", thread_id, tx_id, msg);
                    let request = hydra_node_client_address.clone().into_client_request().unwrap();
                    let (stream, _) = connect_async(request).await.unwrap();
                    let (mut hydra_node_write, mut hydra_node_read) = stream.split();
                    println!("websocket connected to hydra node");

                    let message = Message::text(msg);
                    hydra_node_write.send(message).await.unwrap();
                    // We are awaiting a confirmation on this tx submission
                    pending_submissions_1.lock().await.insert(tx_id.to_string(), thread_id);
                }
            }
        }
    });

    let handle = listen_hydra_udp(
        config.udp_address,
        Arc::clone(&send),
        pending_submissions,
        Arc::clone(&ogmios)
    );

    // map of client send sinks
    let clients = Arc::new(Mutex::new(HashMap::<ThreadId, ChainSyncClient>::new()));

    output_to_ogmios_clients(
        receive,
        Arc::clone(&clients),
        Arc::clone(&ogmios)
    );

    start_ogmios_server(
        config.ogmios_server_address,
        Arc::clone(&ogmios),
        Arc::new(Mutex::new(hydra_client_tx)),
        Arc::clone(&send),
        Arc::clone(&clients),
    ).await;

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

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[arg(long)]
    udp_address: String,
    #[arg(long)]
    hydra_node_client_address: String,
    #[arg(long)]
    ogmios_server_address: String,
    #[arg(long)]
    use_udp: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let config = Config {
        udp_address: args.udp_address, // "0.0.0.0:23457"
        hydra_node_client_address: args.hydra_node_client_address, // "ws://127.0.0.1:4001",
        ogmios_server_address: args.ogmios_server_address, // "127.0.0.1:9001",
    };
    if args.use_udp {
        listen_udp(config).await.unwrap();
    } else {
        listen_websocket(config).await.unwrap();
    }
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
