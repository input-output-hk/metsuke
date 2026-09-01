#!/usr/bin/env nu

# The Key Roster generator, run once an epoch. `query` reads the chain through
# cardano-cli; `generate` is offline and pure, so what a server is handed can be
# checked against what the chain answered without a node in the room.
#
# The server runs neither. It reads the file `generate` writes and re-reads it
# when it changes (ADR 0011).
#
# Pool ids stay in the hex `pool-state` answers with, rather than the bech32 the
# allowlist uses: the file is a transcription of the chain's answer, and the one
# place a pool id is converted stays `metsuke_wire::envelope::PoolId`.

const POOL_ID_HEX = '^[0-9a-f]{56}$'
const LEIOS_KEY_HEX = '^[0-9a-f]{192}$'

def demand [value: any, name: string] {
  if $value == null {
    error make {msg: $"($name) is required"}
  }
  $value
}

# The keys one pool's `pool-state` entry registers: the one in force and the one
# a re-registration has announced for the next epoch, together. ADR 0011 has why
# both.
export def keys-of []: record -> list<string> {
  let entry = $in
  ["poolParams" "futurePoolParams"]
  | each {|field| $entry | get --optional $field }
  | where {|params| $params != null }
  | each {|params|
      let key = $params | get --optional spsLeiosKey.leiosPubKey
      if $key == null {
        error make {msg: "a pool's parameters carry no spsLeiosKey.leiosPubKey"}
      }
      if not ($key =~ $LEIOS_KEY_HEX) {
        error make {msg: $"leiosPubKey ($key) is not 96 bytes of hex"}
      }
      $key
    }
  | uniq
}

# What the server reads: the chain position the answer was taken at, so a roster
# nobody has updated is diagnosable, and every pool against every key it
# registers.
export def as-file [--min-sync: float = 99.8]: record -> string {
  let answer = $in
  let tip = demand ($answer | get --optional tip) "tip"
  let pools = demand ($answer | get --optional pool_state) "pool_state"

  # A node short of the tip lists no key registered past where it has reached,
  # and the epoch and slot it carries make that read as merely older.
  let synced = demand ($tip | get --optional syncProgress) "tip.syncProgress" | into float
  if $synced < $min_sync {
    error make {msg: $"the node has synced ($synced)% of the chain, under the ($min_sync)% asked for"}
  }
  {
    epoch: (demand ($tip | get --optional epoch) "tip.epoch")
    slot: (demand ($tip | get --optional slot) "tip.slot")
    pools: (
      $pools
      | items {|pool_id, entry|
          if not ($pool_id =~ $POOL_ID_HEX) {
            error make {msg: $"($pool_id) is not a 28-byte pool id"}
          }
          [$pool_id ($entry | keys-of)]
        }
      | into record
    )
  }
  | to json
}

def main [] {
  error make {msg: "run `query` or `generate`"}
}

# Both answers in one value, so `generate` reads a file rather than a node. The
# era is a parameter because it is a fact about the network this runs against,
# and `latest` is not it on a network past the cli's latest era.
def "main query" [
  era: string
  --socket-path: path
  --testnet-magic: int
  --mainnet
]: nothing -> string {
  let network = if $mainnet {
    if $testnet_magic != null {
      error make {msg: "--mainnet and --testnet-magic name two different networks"}
    }
    ["--mainnet"]
  } else {
    ["--testnet-magic" (demand $testnet_magic "--testnet-magic" | into string)]
  }
  let common = ["--socket-path" (demand $socket_path "--socket-path")] ++ $network
  {
    tip: (cli [$era "query" "tip"] $common)
    pool_state: (cli [$era "query" "pool-state" "--all-stake-pools"] $common)
  }
  | to json
}

def cli [command: list<string>, common: list<string>]: nothing -> any {
  let arguments = $command ++ $common
  let answer = ^cardano-cli ...$arguments | complete
  if $answer.exit_code != 0 {
    error make {msg: $"cardano-cli ($arguments | str join ' ') failed: ($answer.stderr)"}
  }
  $answer.stdout | from json
}

# Write the file the server reads: beside it, then renamed over it. Taking the
# destination rather than stdout is what keeps a caller from redirecting over a
# roster in use. ADR 0011 has why the swap has to be a rename.
def "main generate" [answer: path, into: path, --min-sync: float = 99.8]: nothing -> nothing {
  let next = $"($into).next"
  open --raw $answer | from json | as-file --min-sync $min_sync | save --force $next
  mv --force $next $into
}
