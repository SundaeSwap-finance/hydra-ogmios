use anyhow::anyhow;
use pallas_primitives::conway::{Tx};
use std::collections::HashMap;

pub struct Ogmios {
    mempool: HashMap<String, Tx>,
    blocks: Vec<Block>,
}

pub struct Block {
    transactions: Vec<Tx>,
}

impl Ogmios {
    pub fn new() -> Ogmios {
        Ogmios{
            mempool: HashMap::new(),
            blocks: vec![],
        }
    }

    pub fn add_transaction(&mut self, tx_id: &String, tx: Tx) {
        self.mempool.insert(tx_id.clone(), tx);
    }

    pub fn new_block(&mut self, hashes: &Vec<String>) -> Result<(), anyhow::Error> {
        let mut block = vec![];
        for hash in hashes {
            match self.mempool.remove(hash) {
                Some(tx_in_block) => {
                    block.push(tx_in_block);
                }
                None => {
                    return Err(anyhow!("tx with hash {} was not found", &hash))
                }
            }
        }
        self.blocks.push(Block{
            transactions: block,
        });
        Ok(())
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
        match ogmios.new_block(&vec![tx_hash_1]) {
            Ok(_txes) => {
            }
            Err(e) => {
                panic!("{}", e)
            }
        }
    }
}
