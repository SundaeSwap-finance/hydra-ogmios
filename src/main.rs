use serde::{Deserialize, Serialize};

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
#[serde(tag = "tag", rename_all_fields = "camelCase")]
enum StateChanged {
    TickObserved {
        chain_slot: u64,
    },
}

fn main() {
    println!("Hello, world!");
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::de::{from_reader, Error};
    use hex;

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
}
