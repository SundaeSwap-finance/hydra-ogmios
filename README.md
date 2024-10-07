# hydra-ogmios

This program provides a similar interface to ogmios, but for hydra instead of
cardano. It connects to the hydra node's websocket port and provides a websocket
server allowing clients to follow transactions on the head in real time and
submit new transactions.

This program works by maintaining a list of "blocks" where a block is treated as
a set of transactions. When it recieves a "TransactionConfirmed" message from
hydra, the transaction is recorded in memory. When it receives a
"SnapshotConfirmed" message, the set of transactions in the snapshot are treated
as a new block which is appended to the list. This program maintains a cursor
for each ogmios client and responds to each "NextBlock" request with the block
corresponding to that client's cursor.

## UDP sink example implementation

The program can optionally obtain hydra events via UDP stream instead of by
connecting to the hydra node's websocket API. This requires running a hydra node
with a custom UDP event sink. An example implementation can be found here:

https://github.com/SundaeSwap-finance/hydra/commit/90ffe09f649c44fadd1683432d764b5eeb9086b6

# Example FindIntersect request
```
{
  "jsonrpc": "2.0",
  "method": "findIntersection",
  "params": {
    "points": [
      "origin"
    ]
  }
}
```

# Example FindIntersect response

```
{
  "jsonrpc": "2.0",
  "method": "findIntersection",
  "result": {
    "intersection": {
      "id":
 "d7455525a5d411a50ecb82883e4c2009009894b398a0f0d77800de779b640c4943d427514249dada8e21895dd0ff936c5e719a82a5c3b37d22fe6b6bf77ff8cf",
      "slot": 1727956570
    },
    "tip": {
      "id": "cf308649e0df784159616ec53d6cde3aae3efe487123c5abf92c1a5c501a8b552dd4d2c4518913002f8969887074b9f091f1fab832066a2bf637af2e1e80ac94",
      "slot": 1727956692
    }
  }
}
```

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
