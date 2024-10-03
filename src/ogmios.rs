use anyhow::{Context, Result, anyhow};
use pallas_primitives::conway::{Tx};
use std::collections::HashMap;
use std::cmp::Ordering;
use blake2::{Blake2b512, Digest};
use serde_json::{Value};
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointStruct {
    pub slot: u64,
    pub hash: String,
    pub height: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Point {
    Point(PointStruct),
    Origin,
}

impl PartialOrd for Point {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Point::Origin, Point::Origin) => Some(Ordering::Equal),
            (Point::Origin, _) => Some(Ordering::Less),
            (Point::Point(_), Point::Origin) => Some(Ordering::Greater),
            (Point::Point(ps), Point::Point(ps2)) => {
                if ps.slot == ps2.slot {
                    if ps.hash == ps2.hash {
                        Some(Ordering::Equal)
                    } else {
                        None
                    }
                } else {
                    Some(ps.slot.cmp(&ps2.slot))
                }
            }
        }
    }
}

impl TryFrom<Value> for Point {
    type Error = anyhow::Error;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        if let Some(object) = value.as_object() {
            let slot = object["slot"].as_u64().context("Invalid slot")?;
            let id = object["id"].as_str().context("Invalid id")?;
            return Ok(Point::Point(PointStruct{
                slot: slot,
                hash: id.to_string(),
                height: None,
            }))
        } else if let Some(string) = value.as_str() {
            if string == "origin" {
                return Ok(Point::Origin)
            } else {
                return Err(anyhow!("unexpected point string: {}; expected \"origin\"", string));
            }
        } else {
            return Err(anyhow!("unexpected point: expected object or string: {}", value));
        }
    }
}

pub fn encode_point(p: &Point) -> Value {
    match p {
        Point::Point(ps) => {
            json!({
                "slot": ps.slot,
                "id": ps.hash,
            })
        }
        Point::Origin => {
            json!("origin")
        }
    }
}

pub struct Ogmios {
    mempool: HashMap<String, Tx>,
    blocks: Vec<Block>,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub height: u64,
    pub hash: Vec<u8>,
    pub time: u64,
    pub transactions: Vec<(String, Tx)>,
}

impl Block {
    pub fn point(&self) -> PointStruct{
        PointStruct {
            slot: self.time,
            hash: hex::encode(&self.hash),
            height: Some(self.height),
        }
    }
}

pub enum FindIntersection {
    Found((Point, Point)),
    NotFound(Point),
}

impl Ogmios {
    pub fn new() -> Ogmios {
        Ogmios{
            mempool: HashMap::new(),
            blocks: vec![],
        }
    }

    pub fn find_intersection(&self, points: &Vec<Point>) -> FindIntersection {
        let mut points = points.clone();
        points.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        let mut block_ix = 0;
        let mut points_ix = 0;
        let mut latest = None;
        let tip = self.blocks.last().clone();
        let tip_point = match tip {
            Some(tip) => Point::Point(tip.point()),
            None => Point::Origin,
        };
        loop {
            if points_ix >= points.len() {
                break;
            }
            if block_ix >= self.blocks.len() {
                break;
            }
            match &points[points_ix] {
                Point::Point(ps) => {
                    let block = &self.blocks[block_ix];
                    if block.time < ps.slot {
                        latest = Some(block_ix);
                        block_ix += 1;
                    } else if block.time == ps.slot {
                        latest = Some(block_ix);
                        points_ix += 1;
                    } else if block.time > ps.slot {
                        return FindIntersection::NotFound(tip_point)
                    }
                }
                Point::Origin => {
                    latest = Some(0);
                    points_ix += 1;
                }
            }
        }
        if let Some(latest) = latest {
            FindIntersection::Found((Point::Point(self.blocks[latest].point()), tip_point))
        } else {
            FindIntersection::Found((Point::Origin, tip_point))
        }
    }

    pub fn add_transaction(&mut self, tx_id: &String, tx: Tx) {
        self.mempool.insert(tx_id.clone(), tx);
        println!("inserted txid: {}", tx_id)
    }

    pub fn new_block(&mut self, hashes: &Vec<String>, unix_time: u64) -> Result<(), anyhow::Error> {
        let mut block = vec![];
        let mut hasher = Blake2b512::new();
        for hash in hashes {
            hasher.update(hash);
            match self.mempool.remove(hash) {
                Some(tx_in_block) => {
                    println!("found tx in mempool: {}", hash);
                    block.push((hash.clone(), tx_in_block));
                }
                None => {
                    return Err(anyhow!("tx with hash {} was not found in {:?}", &hash, self.mempool.keys().collect::<Vec<_>>()))
                }
            }
        }
        self.blocks.push(Block{
            height: self.blocks.len() as u64,
            hash: hasher.finalize().to_vec(),
            time: unix_time,
            transactions: block,
        });
        Ok(())
    }

    pub fn get_block(&self, index: usize) -> Option<(&Block, &Block)> {
        if index >= self.blocks.len() {
            None
        } else {
            Some((&self.blocks[index], &self.blocks[self.blocks.len()-1]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use hex;
    use std::fs;

    #[test]
    fn test_ogmios() {
        let tx_hash_1 = "0bd8311167aba722516deea66ca858419d6c4c55262392592550016b15fcf325".to_string();
        let tx_1_hex = fs::read_to_string("testdata/tx_1").unwrap();
        let tx_1_bytes = hex::decode(tx_1_hex).unwrap();
        let tx_1 = minicbor::decode(&tx_1_bytes).unwrap();
        let mut ogmios = Ogmios::new();
        ogmios.add_transaction(&tx_hash_1, tx_1);
        match ogmios.new_block(&vec![tx_hash_1], 0) {
            Ok(_txes) => {
            }
            Err(e) => {
                panic!("{}", e)
            }
        }
    }
}
