#!/usr/bin/env nu

use std/assert
use roster.nu *

# fixtures/query-answer.json is a recording: the two answers `main query` makes,
# taken from the local Leios devnet (docs/research/leios-devnet.md) with
# cardano-cli 11.1.0.0. Every pool in it has one registered key and no announced
# one, so the recording covers the roster's shape and not a rotation in flight.
# Re-record it from a node at the tip: its syncProgress has to clear `as-file`'s
# own default or every test here refuses it.
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
  assert equal $roster.epoch 2
  assert equal $roster.slot 2605
}

# The rotation case, which the recording has no announced registration for. The
# shape is not guessed: cardano-cli encodes both fields from one `Maybe
# PoolParams` (`Cardano.CLI.Type.Common`, the `ToJSON (Params crypto)`
# instance), so an announced registration is the recorded entry's own
# `poolParams` under the other name, which is what this builds.
def both-the-registered-and-the-announced-key-are-listed [] {
  let entry = recorded | get pool_state | get $POOL
  let announced = $entry | upsert futurePoolParams (
    $entry.poolParams | upsert spsLeiosKey.leiosPubKey $OTHER_KEY
  )

  assert equal ($announced | keys-of) [$KEY $OTHER_KEY]
}

# A pool that registered the same key twice is one key, not two: what the server
# checks is membership.
def a-key-announced-unchanged-is-listed-once [] {
  let entry = recorded | get pool_state | get $POOL
  let unchanged = $entry | upsert futurePoolParams $entry.poolParams

  assert equal ($unchanged | keys-of) [$KEY]
}

def a-pool-with-no-leios-key-is-an-error [] {
  let entry = recorded | get pool_state | get $POOL
  let keyless = $entry | update poolParams { reject spsLeiosKey }

  assert error { $keyless | keys-of }
}

def a-key-that-is-not-96-bytes-is-an-error [] {
  let entry = recorded | get pool_state | get $POOL
  let short = $entry | upsert poolParams.spsLeiosKey.leiosPubKey "abcd"

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

# The recording is a caught-up node, so the threshold is what decides rather
# than the shape of the answer.
def the-threshold-is-what-refuses-an-answer [] {
  assert equal (generated | get epoch) 2
  assert error { recorded | as-file --min-sync 99.9 }
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
  assert equal (open $into | get epoch) 2
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
  print "roster: ok"
}
