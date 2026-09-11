#!/usr/bin/env nu

use std/assert
use roster.nu *

# fixtures/query-answer.json is a recording: the two answers `main query` makes,
# taken from the local Leios devnet (docs/research/leios-devnet.md) with
# cardano-cli 11.2.2.0. Every pool in it has one registered key and no announced
# one, so the recording covers the roster's shape and not a rotation in flight.
# Re-record it from a node at the tip: its syncProgress has to clear `as-file`'s
# own default or every test here refuses it, and `query` names the era because
# `latest` is Conway while the devnet forges Dijkstra.
const ANSWER = path self fixtures/query-answer.json
const ROSTER = path self roster.nu

const POOL = "eb8865c72876f93e07d3db55c14c03a542afa1ab8ad83065723a3204"
const KEY = "ae23eff571532a2e0542b2f7a4e8ae59c1dc40aafdec0ce3a3e2d36d0240e1bbb02d417f56388cd44bd553679e6c6dc40173b5c7dcb94ba6f08a1ba20796d0c243ea2a5e913a8dd0dfce27822405b2daa53c60557645d9320908690e888eefcf"
const OTHER_KEY = "948a726e70c21af0535f0c5b58b7b1bac46e94300d348b8c65abf4ed86998714192674ef612d52de5647c5eb4bc3a5db0f44f1f02a61628805000f0719428578c36569318dd8a03c2ca3d41e318f6bcd8c0b8cb6e3c6fae99f389a0b465a9921"

def recorded []: nothing -> record {
  open --raw $ANSWER | from json
}

def generated []: nothing -> record {
  recorded | as-file | from json
}

def the-recorded-answer-becomes-a-roster [] {
  let roster = generated
  assert equal ($roster.pools | get $POOL) [$KEY]
  assert equal ($roster.pools | columns | length) 3
}

# A roster nobody has updated is a roster that refuses whoever rotated, so where
# it was taken has to travel with it.
def the-tip-the-answer-was-taken-at-travels-with-it [] {
  let roster = generated
  assert equal $roster.epoch 0
  assert equal $roster.slot 144
}

# The announced half in the shape cardano-cli answers it in, which is not the
# registered half's. `futurePoolParams` is a ledger StakePoolParams, whose
# hand-written ToJSON (`Cardano.Ledger.State.StakePool`) emits the key as a flat
# `blsKey`; `poolParams` is a StakePoolState, whose JSON comes from its own field
# names and nests the key under `spsBlsKey.bksKey`. Building this half by copying
# the other is what stops holding when they diverge.
def announced-half [key: string]: record -> record {
  let entry = $in
  {
    blsKey: {
      blsPubKey: $key
      blsPossessionProof: $entry.poolParams.spsBlsKey.bksKey.blsPossessionProof
    }
  }
}

# The rotation case, which the recording has no announced registration for.
def both-the-registered-and-the-announced-key-are-listed [] {
  let entry = recorded | get pool_state | get $POOL
  let announced = $entry | upsert futurePoolParams ($entry | announced-half $OTHER_KEY)

  assert equal ($announced | keys-of) [$KEY $OTHER_KEY]
}

# A pool that registered the same key twice is one key, not two: what the server
# checks is membership.
def a-key-announced-unchanged-is-listed-once [] {
  let entry = recorded | get pool_state | get $POOL
  let unchanged = $entry | upsert futurePoolParams ($entry | announced-half $KEY)

  assert equal ($unchanged | keys-of) [$KEY]
}

def a-pool-with-no-leios-key-is-an-error [] {
  let entry = recorded | get pool_state | get $POOL
  let keyless = $entry | update poolParams { reject spsBlsKey }

  assert error { $keyless | keys-of }
}

def a-key-that-is-not-96-bytes-is-an-error [] {
  let entry = recorded | get pool_state | get $POOL
  let short = $entry | upsert poolParams.spsBlsKey.bksKey.blsPubKey "abcd"

  assert error { $short | keys-of }
}

def an-answer-missing-a-half-is-an-error [] {
  assert error { {pool_state: {}} | as-file }
  assert error { {tip: {epoch: 1, slot: 2}} | as-file }
}

def a-node-still-catching-up-is-an-error [] {
  let syncing = recorded | upsert tip.syncProgress "12.34"

  assert error { $syncing | as-file }
}

# One answer, accepted under one threshold and refused under another, so what
# decides is the threshold and not the shape of the answer. The figure is set
# here rather than taken from the recording, which would only hold while a
# re-recording kept landing in the narrow band between the two.
def the-threshold-is-what-refuses-an-answer [] {
  let behind = recorded | upsert tip.syncProgress "99.85"

  assert equal ($behind | as-file --min-sync 99.8 | from json | get epoch) 0
  assert error { $behind | as-file --min-sync 99.9 }
}

# Refused rather than assumed caught up: a cli that stops reporting it must not
# silently start writing rosters off a node nobody checked.
def an-answer-with-no-sync-figure-is-an-error [] {
  let quiet = recorded | update tip { reject syncProgress }

  assert error { $quiet | as-file }
}

def a-key-that-is-not-a-pool-id-is-an-error [] {
  let answer = recorded
  let renamed = {
    tip: $answer.tip
    pool_state: {"not-a-pool-id": ($answer.pool_state | get $POOL)}
  }

  assert error { $renamed | as-file }
}

# The swap the server's change detection rests on: every `generate` leaves the
# name pointing at a new inode, and leaves no half-written file behind.
def each-generate-replaces-the-file-by-rename [] {
  let dir = mktemp --directory
  let into = $dir | path join roster.json

  ^$nu.current-exe $ROSTER generate $ANSWER $into
  let first = ls --long $into | get 0.inode
  ^$nu.current-exe $ROSTER generate $ANSWER $into
  let second = ls --long $into | get 0.inode

  assert not equal $first $second
  assert equal (ls $dir | get name | path basename) ["roster.json"]
  assert equal (open $into | get epoch) 0
  rm --recursive --force $dir
}

# A cardano-cli that fails its first `failures` calls and answers the recording
# after that, so `query`'s retry can be exercised without a node. The counter
# is a file because each call is its own process, and it is shared across the
# two queries one run makes: the tip query spends the failures, the pool-state
# query that follows finds the fake already answering.
def fake-cli [directory: path, failures: int]: nothing -> nothing {
  let answer = recorded
  $answer.tip | to json | save --force ($directory | path join tip.json)
  $answer.pool_state | to json | save --force ($directory | path join pool-state.json)
  $failures | into string | save --force ($directory | path join failures)
  let script = $directory | path join cardano-cli
  # A raw string and $FAKE_DIR rather than interpolation: every $ below is the
  # shell's, and nothing here has to be escaped past nu to reach it.
  '#!/bin/sh
count=$(cat "$FAKE_DIR/count" 2>/dev/null || echo 0)
count=$((count + 1))
echo "$count" > "$FAKE_DIR/count"
if [ "$count" -le "$(cat "$FAKE_DIR/failures")" ]; then
  echo "Network.Socket.connect: does not exist" >&2
  exit 1
fi
case "$*" in
  *pool-state*) cat "$FAKE_DIR/pool-state.json" ;;
  *) cat "$FAKE_DIR/tip.json" ;;
esac
' | save --force $script
  chmod +x $script
}

# Prepended to PATH rather than replacing it: the fake is a shell script, so it
# needs the coreutils the real environment has, and a PATH holding only the
# fake makes its every read fail into the fallback instead.
def query-with [directory: path, --attempts: int]: nothing -> record {
  with-env {PATH: ([$directory] ++ $env.PATH), FAKE_DIR: $directory} {
    ^$nu.current-exe $ROSTER query babbage --socket-path /dev/null --testnet-magic 42 --attempts $attempts --retry-backoff 10ms
  }
  | complete
}

# The failure this exists for: a node up but not answering yet, which an
# activation beside it is enough to cause. One transient refusal is absorbed
# and the run still writes a roster.
def a-query-that-fails-once-is-retried [] {
  let dir = mktemp --directory
  fake-cli $dir 1

  let run = query-with $dir --attempts 3

  assert equal $run.exit_code 0
  assert equal ($run.stdout | from json | get tip.epoch) 0
  assert str contains $run.stderr "attempt 1 of 3 failed"
  rm --recursive --force $dir
}

# And the alarm keeps its meaning: a socket that never answers still fails, so
# a genuinely unreachable node is not retried into silence.
def a-query-that-never-answers-is-still-an-error [] {
  let dir = mktemp --directory
  fake-cli $dir 99

  let run = query-with $dir --attempts 3

  assert not equal $run.exit_code 0
  assert str contains $run.stderr "does not exist"
  rm --recursive --force $dir
}

# One attempt is the old behaviour, and asking for none is refused rather than
# read as one.
def the-attempt-count-is-what-bounds-the-retry [] {
  let dir = mktemp --directory
  fake-cli $dir 1

  let once = query-with $dir --attempts 1
  assert not equal $once.exit_code 0
  assert str contains $once.stderr "does not exist"

  let none = query-with $dir --attempts 0
  assert not equal $none.exit_code 0
  assert str contains $none.stderr "cannot be under 1"
  rm --recursive --force $dir
}

def main [] {
  the-recorded-answer-becomes-a-roster
  each-generate-replaces-the-file-by-rename
  the-tip-the-answer-was-taken-at-travels-with-it
  both-the-registered-and-the-announced-key-are-listed
  a-key-announced-unchanged-is-listed-once
  a-pool-with-no-leios-key-is-an-error
  a-key-that-is-not-96-bytes-is-an-error
  an-answer-missing-a-half-is-an-error
  a-key-that-is-not-a-pool-id-is-an-error
  a-node-still-catching-up-is-an-error
  the-threshold-is-what-refuses-an-answer
  an-answer-with-no-sync-figure-is-an-error
  a-query-that-fails-once-is-retried
  a-query-that-never-answers-is-still-an-error
  the-attempt-count-is-what-bounds-the-retry
  print "roster: ok"
}
