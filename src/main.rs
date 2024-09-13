use serde::{Deserialize, Serialize};
use pallas_primitives;
use minicbor;

const TICK_OBSERVED_1: &str = "a2676576656e744964191eaf6c73746174654368616e676564a269636861696e536c6f741941ee637461676c5469636b4f62736572766564";

// src/Hydra/Events.hs 'StateEvent'
// This is the type sent to EventSinks.
#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Event {
    event_id: u64,
    state_changed: StateChanged,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct TransactionReceivedTx {
    cbor_hex: String,
    description: String,
    tx_id: String,
    r#type: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "tag", rename_all_fields = "camelCase")]
enum StateChanged {
    TickObserved {
        chain_slot: u64,
    },
    /*
    CommittedUTxO {
        party: Party,
        committed_UTxO: UTxO,
        chain_state: ChainState,
    },
    */
    TransactionReceived {
        tx: TransactionReceivedTx,
    }
}

fn main() {
    println!("Hello, world!");
}

#[cfg(test)]
mod tests {
    use super::*;

    use ciborium::de::{from_reader, Error};
    use hex;
    use serde_json::{from_str};
    use std::fs;

    #[test]
    fn tick_observed_can_parse() {
        let tick_observed_1_raw = hex::decode(TICK_OBSERVED_1).unwrap();
        let result: Result<Event, Error<std::io::Error>> = from_reader(&tick_observed_1_raw[..]);
        match result {
            Ok(r) => {
                println!("Event: {:?}", r)
            }
            Err(e) => {
                panic!("{}", e)
            }
        }
    }

    #[test]
    fn transaction_received_can_parse_json() {
        let transaction_received = fs::read_to_string("testdata/transaction_received.json")
            .expect("couldn't read file");
        let result: Event = serde_json::from_str(&transaction_received)
            .expect("couldn't parse event");
        match result.state_changed {
            StateChanged::TransactionReceived { tx } => {
                let tx_cbor_raw = hex::decode(tx.cbor_hex).unwrap();
                let tx: pallas_primitives::conway::Tx = minicbor::decode(tx_cbor_raw)
                    .expect("couldn't decode cbor");
            }
            _ => {
            }
        }
        println!("Event: {:?}", result);
    }
}
