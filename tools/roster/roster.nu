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
#
# A path per half rather than one for both: cardano-cli answers `poolParams` as
# a ledger StakePoolState, whose JSON keys are that record's own field names,
# and `futurePoolParams` as a StakePoolParams, whose instance is written by
# hand. The two carry the same key under different names and different nesting,
# so a half read with the other's path reads as a pool that registered nothing.
# A half carrying no key lists none rather than failing here: what to do about a
# pool the chain registers no key for is a decision about the whole answer, and
# `as-file` is where it is taken. A key that is present and the wrong shape
# still fails, because that is not a pool without a key.
export def keys-of []: record -> list<string> {
  let entry = $in
  [
    ($entry | get --optional poolParams.spsBlsKey.bksKey.blsPubKey)
    ($entry | get --optional futurePoolParams.blsKey.blsPubKey)
  ]
  | where {|key| $key != null }
  | each {|key|
      if not ($key =~ $LEIOS_KEY_HEX) {
        error make {msg: $"blsPubKey ($key) is not 96 bytes of hex"}
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
  let listed = (
    $pools
    | items {|pool_id, entry|
        if not ($pool_id =~ $POOL_ID_HEX) {
          error make {msg: $"($pool_id) is not a 28-byte pool id"}
        }
        {pool: $pool_id, keys: ($entry | keys-of)}
      }
  )

  # A pool the chain registers no BLS key for is left out rather than refused:
  # it cannot sign with a key it does not have, so failing the run would only
  # deny every pool that can. A roster with no pool in it at all is the other
  # thing, either the path this reads having moved as it did at w36 or
  # pool-state answering nothing, and that is loud whichever it was: an empty
  # roster refuses the whole network and reads exactly like a quiet one.
  let keyed = $listed | where {|pool| ($pool.keys | length) > 0 }
  if ($keyed | length) == 0 {
    error make {
      msg: $"no pool of the ($listed | length) answered lists a BLS key, so either pool-state returned nothing or the path this reads has moved"
    }
  }

  {
    epoch: (demand ($tip | get --optional epoch) "tip.epoch")
    slot: (demand ($tip | get --optional slot) "tip.slot")
    pools: ($keyed | each {|pool| [$pool.pool $pool.keys] } | into record)
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
  # Per query, not per run, and both queries get their own. Three attempts ten
  # seconds apart is the transient window `cli` describes; a node that needs
  # longer is one the next tick should find rather than this run wait out.
  #
  # These stage work rather than bound what a roster may contain, so the
  # deployment takes them as they are and the NixOS module passes neither.
  # Widening them past a unit's start timeout is what that would risk: the
  # defaults spend at most twenty seconds a query, well inside the ninety a
  # `Type=oneshot` service is given, and a budget over that is SIGTERMed
  # before the last attempt can report why.
  --attempts: int = 3
  --retry-backoff: duration = 10sec
]: nothing -> string {
  if $attempts < 1 {
    error make {msg: "--attempts is how many times a query may run, so it cannot be under 1"}
  }
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
    tip: (cli [$era "query" "tip"] $common $attempts $retry_backoff)
    pool_state: (cli [$era "query" "pool-state" "--all-stake-pools"] $common $attempts $retry_backoff)
  }
  | to json
}

# One cardano-cli call, retried. A node that is up but not answering yet is the
# ordinary case here: the timer fires against a node this script does not
# supervise, and an activation that restarts something beside it is enough to
# catch one mid-answer. Bounded, so a socket that is genuinely gone still fails
# and still says so on the last attempt. Each retry goes to stderr rather than
# being swallowed, because a run that recovered and a run that never had to are
# worth telling apart in the journal.
#
# Every non-zero exit is retried, so a wrong era or a mistyped magic waits out
# the same budget before it is reported. That is the trade: telling those apart
# means matching on cardano-cli's stderr, which would go stale under it.
def cli [
  command: list<string>
  common: list<string>
  attempts: int
  backoff: duration
]: nothing -> any {
  let arguments = $command ++ $common
  mut attempt = 1
  loop {
    let answer = ^cardano-cli ...$arguments | complete
    if $answer.exit_code == 0 {
      return ($answer.stdout | from json)
    }
    if $attempt >= $attempts {
      error make {msg: $"cardano-cli ($arguments | str join ' ') failed: ($answer.stderr)"}
    }
    print --stderr $"cardano-cli ($command | str join ' ') attempt ($attempt) of ($attempts) failed, retrying in ($backoff): ($answer.stderr | str trim)"
    sleep $backoff
    $attempt += 1
  }
}

# Write the file the server reads: beside it, then renamed over it. Taking the
# destination rather than stdout is what keeps a caller from redirecting over a
# roster in use. ADR 0011 has why the swap has to be a rename.
def "main generate" [answer: path, into: path, --min-sync: float = 99.8]: nothing -> nothing {
  let next = $"($into).next"
  let answered = open --raw $answer | from json
  let roster = $answered | as-file --min-sync $min_sync
  $roster | save --force $next
  mv --force $next $into

  # A pool left out for registering no key is not an error, so this line is the
  # only place it is visible. A count rather than a list: what it answers is
  # whether the roster still covers the network, and a timer's stderr is read in
  # the journal after the fact rather than watched.
  let covered = $roster | from json | get pools | columns | length
  let omitted = ($answered.pool_state | columns | length) - $covered
  if $omitted > 0 {
    print --stderr $"($omitted) of ($omitted + $covered) pools list no BLS key and are not in the roster"
  }
}
