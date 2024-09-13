use anyhow::{Context, Result};
use serde_json::Value;

use crate::utxo::UTxO;

#[allow(dead_code)]
#[derive(Debug)]
pub struct SnapshotConfirmed {
    pub snapshot: Snapshot,
    pub signatures: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct Snapshot {
    pub head_id: String,
    pub confirmed_transactions: Vec<Vec<u8>>,
    pub snapshot_number: u64,
    pub utxo: Vec<UTxO>,
    pub version: u64,
}

impl TryFrom<Value> for SnapshotConfirmed {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let snapshot = Snapshot::try_from(value["snapshot"].clone())?;

        let signatures = value["signatures"]
            .as_object()
            .context("invalid signatures object")?["multiSignature"]
            .as_array()
            .context("Invalid multiSignatures")?
            .iter()
            .map(|s| s.as_str().context("invalid str").map(|s| s.to_string()))
            .collect::<Result<Vec<String>>>()?;

        Ok(SnapshotConfirmed {
            snapshot: snapshot,
            signatures: signatures,
        })
    }
}

impl TryFrom<Value> for Snapshot {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        let head_id = value["headId"]
            .as_str()
            .context("Invalid head_id")?
            .to_owned();
        let confirmed_transactions = value["confirmedTransactions"]
            .as_array()
            .context("Invalid confirmedTransactions")?
            .iter()
            .map(|s| {
                s.as_str()
                    .context("invalid confirmedTransaction")
                    .and_then(|s| hex::decode(s).context("failed to hex decode"))
            })
            .collect::<Result<Vec<Vec<u8>>>>()?;
        let snapshot_number = value["snapshotNumber"]
            .as_u64()
            .context("Invalid snapshotNumber")?;
        let utxo = value["utxo"]
            .as_object()
            .context("Invalid utxo")?
            .iter()
            .map(|(key, value)| UTxO::try_from_value(key, value))
            .collect::<Result<Vec<UTxO>>>()?;
        let version = value["version"].as_u64().context("Invalid version")?;

        Ok(Snapshot {
            head_id: head_id.to_string(),
            confirmed_transactions,
            snapshot_number,
            utxo,
            version,
        })
    }
}
