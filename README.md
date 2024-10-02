# hydra-ogmios

This program provides a similar interface to ogmios, but for hydra instead of
cardano. It connects to the hydra node's websocket port and a custom UDP sink,
and provides a websocket server allowing clients to follow transactions on the
head in real time and submit new transactions.

This program works by maintaining a list of "blocks" where a block is treated as
a set of transactions. When it recieves a "TransactionConfirmed" message from
the UDP sink, the transaction is recorded in memory. When it receives a
"SnapshotConfirmed" message, the set of transactions in the snapshot are treated
as a new block which is appended to the list. This program maintains a cursor
for each ogmios client and responds to each "NextBlock" request with the block
corresponding to that client's cursor.

# Example NextBlock request

```
{
  "jsonrpc": "2.0",
  "method": "nextBlock",
  "id": null
}
```

# Example NextBlock response

Note: In place of the slot, we use the time at which the SnapshotConfirmed
message was received. 

```
{
  "jsonrpc": "2.0",
  "method": "nextBlock",
  "result": {
    "block": {
      "era": "conway",
      "height": 0,
      "id": "2fb0ab6aa09a3400a6613ad6b4f05fedb9d7de28f3b813cf89144ba7d1ea1c4bbd142480caf341c085ef7bd1a105b7c2ec43e9100647b15cadab903c7cd4ce0f",
      "slot": 1727746433,
      "transactions": [
        {
          "fee": {
            "ada": {
              "lovelace": 0
            }
          },
          "id": "280255d00772a007db69ca914fbd6695ef5ed160489d36debe95fd0f4f789bb4",
          "inputs": [
            {
              "index": 0,
              "transaction": {
                "id": "c9a5fb7ca6f55f07facefccb7c5d824eed00ce18719d28ec4c4a2e4041e85d97"
              }
            }
          ],
          "network": "testnet",
          "outputs": [
            {
              "address": "605e4e214a6addd337126b3a61faad5dfe1e4f14f637a8969e3a05eefd",
              "datumHash": null,
              "value": {
                "ada": {
                  "lovelace": 2450000
                }
              }
            },
            {
              "address": "6069830961c6af9095b0f2648dff31fa9545d8f0b6623db865eb78fde8",
              "datumHash": null,
              "value": {
                "ada": {
                  "lovelace": 97550000
                }
              }
            }
          ],
          "spends": "inputs",
          "validityInterval": {
            "invalidAfter": null,
            "invalidBefore": null
          }
        }
      ],
      "type": "praos"
    },
    "direction": "forward",
    "tip": {
      "height": 1,
      "id": "76b365ef938e03b56a35b2b94deea61d7aca77c4119c3cc1e93c0084db0f3a1bf8c6ddcc91daf82af8f0a69ee97e35f1f7a4d496981a208772a195d2cd2fd5f4",
      "slot": 1727842225
    }
  }
}
```

# Example SubmitTransaction request

SubmitTransaction request is the same as ogmios:

```
{
  "jsonrpc": "2.0",
  "method": "submitTransaction",
  "params": {
    "transaction": {
      "cbor": "84a300d9010281825820c9a5fb7ca6f55f07facefccb7c5d824eed00ce18719d28ec4c4a2e4041e85d9700018282581d605e4e214a6addd337126b3a61faad5dfe1e4f14f637a8969e3a05eefd1a0025625082581d6069830961c6af9095b0f2648dff31fa9545d8f0b6623db865eb78fde81a05d07eb00200a100d9010281825820f953b2d6b6f319faa9f8462257eb52ad73e33199c650f0755e279e21882399c05840d9226a78f6a2762ea2d914b00da642e42b8480391a41510253f50eacb4d7c694a431aa333f369c75b08e5c9bbd6e5642e3a876999db6f585792869dd36d41207f5f6"
    }
  }
}
```

# Example SubmitTransaction response (success)

```
{
  "jsonrpc": "2.0",
  "method": "submitTransaction",
  "result": {
    "transaction": {
      "id": "40fe97a9c5628fe75362044c7918905ec8a59b26ef2c94283f03e6a8d5193aef"
    }
  }
}
```

# Example SubmitTransaction response (error)

Note: code and data fields in the error case are not implemented; the message field is just a
copy of the hydra node's response.

```
{
  "jsonrpc": "2.0",
  "method": "submitTransaction",
  "result": {
    "error": {
      "code": null,
      "data": null,
      "message": "ApplyTxError (ConwayUtxowFailure (UtxoFailure (ValueNotConservedUTxO (MaryValue (Coin 0) (MultiAsset (fromList []))) (MaryValue (Coin 100000000) (MultiAsset (fromList []))))) :| [ConwayUtxowFailure (UtxoFailure (BadInputsUTxO (fromList [TxIn (TxId {unTxId = SafeHash \"c9a5fb7ca6f55f07facefccb7c5d824eed00ce18719d28ec4c4a2e4041e85d97\"}) (TxIx {unTxIx = 0})])))])"
    }
  }
}
```
