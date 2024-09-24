use pallas_codec::utils::{Bytes};
use pallas_primitives::alonzo;
use pallas_primitives::babbage::{PseudoPostAlonzoTransactionOutput};
use pallas_primitives::conway;
use pallas_primitives::conway::{Tx, PseudoDatumOption, TransactionInput, TransactionOutput, PseudoTransactionOutput};
use serde_json::Value;
use serde_json::json;
use std::ops::Deref;

pub fn encode_json_tx(transaction_id: &str, tx: &Tx) -> Value {
    json!({
        "id": transaction_id,
        "spends": "inputs",
        "inputs": tx.transaction_body.inputs.iter().map(|i| encode_json_transaction_input(&i)).collect::<Vec<_>>(),
        "outputs": tx.transaction_body.outputs.iter().map(|o| encode_json_transaction_output(&o)).collect::<Vec<_>>(),
        "fee": json!({
            "ada": json!({
                "lovelace": tx.transaction_body.fee,
            }),
        }),
        "validityInterval": json!({
            "invalidBefore": tx.transaction_body.validity_interval_start,
            "invalidAfter": tx.transaction_body.ttl,
        }),
        "network": "testnet",
    })
}

fn encode_json_transaction_input(input: &TransactionInput) -> Value {
    json!({
        "index": input.index,
        "transaction": json!({
            "id": input.transaction_id,
        }),
    })
}

fn encode_json_transaction_output(output: &TransactionOutput) -> Value {
    match output {
        PseudoTransactionOutput::Legacy(l) => {
            json!({
                "address": l.address,
                "value": encode_json_alonzo_value(&l.amount),
                "datumHash": l.datum_hash,
            })
        },
        PseudoTransactionOutput::PostAlonzo(o) => {
            let (datum_hash, datum) = match &o.datum_option {
                Some(PseudoDatumOption::Hash(datum_hash)) => {
                    (json!(datum_hash), json!(null))
                },
                Some(PseudoDatumOption::Data(plutus_data)) => {
                    (json!(null), json!(plutus_data))
                },
                None => (json!(null), json!(null))
            };
            json!({
              "address": o.address,
              "value": encode_json_value(&o.value),
              "datumHash": datum_hash,
              "datum": datum,
            })
        }
    }
}

fn encode_json_alonzo_value(value: &alonzo::Value) -> Value {
    match value {
        alonzo::Value::Coin(c) => {
            json!({ "ada": json!({ "lovelace": c }) })
        },
        alonzo::Value::Multiasset(c, ma) => {
            let mut m = serde_json::Map::new();
            m.insert("ada".to_string(), json!({ "lovelace": c }));
            for asset in ma.clone().to_vec() {
                let policy = asset.0;
                let mut policy_assets = serde_json::Map::new();
                for (token_name, amount) in asset.1.to_vec() {
                    policy_assets.insert(hex::encode(token_name.deref()), json!(amount));
                }
                m.insert(hex::encode(policy), serde_json::Value::Object(policy_assets));
            }
            serde_json::Value::Object(m)
        }
    }
}

fn encode_json_value(value: &conway::Value) -> Value {
    match value {
        conway::Value::Coin(c) => {
            json!({ "ada": json!({ "lovelace": c }) })
        },
        conway::Value::Multiasset(c, ma) => {
            let mut m = serde_json::Map::new();
            m.insert("ada".to_string(), json!({ "lovelace": c }));
            for asset in ma.clone().to_vec() {
                let policy = asset.0;
                let mut policy_assets = serde_json::Map::new();
                for (token_name, amount) in asset.1.to_vec() {
                    policy_assets.insert(hex::encode(token_name.deref()), json!(amount));
                }
                m.insert(hex::encode(policy), serde_json::Value::Object(policy_assets));
            }
            serde_json::Value::Object(m)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_test() {
        let (_, addr) = bech32::decode("addr1q9d34spgg2kdy47n82e7x9pdd6vql6d2engxmpj20jmhuc2047yqd4xnh7u6u5jp4t0q3fkxzckph4tgnzvamlu7k5psuahzcp").unwrap();
        let v = encode_json_transaction_output(&PseudoTransactionOutput::PostAlonzo(PseudoPostAlonzoTransactionOutput{
            address: Bytes::try_from(addr).unwrap(),
            value: pallas_primitives::conway::Value::Coin(0),
            datum_option: None,
            script_ref: None,
        }));
        assert_eq!(serde_json::to_string(&v).unwrap(), "")
    }
}
