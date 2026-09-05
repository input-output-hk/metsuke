#!/usr/bin/env nu

use std/assert
use allowlist.nu *

# fixtures/registered-codes.csv is mostly hand-written in the shape `psql --csv`
# answers REGISTERED_CODES with, so it can carry rows a real answer would not:
# the three punctuation classes a code may use, and the malformed rows the
# reader drops. Its last row and Dai are one pair recorded from a real
# db-sync, which is the shape every code on chain actually has.
const APPLICATIONS = path self fixtures/applications.csv
const REGISTERED = path self fixtures/registered-codes.csv

const GENERATOR = path self allowlist.nu

const ADA_FIRST = "pool1ygknss6wt9jx7759jzd6dvduclfdm68nlcy3g8e2x4qykuzgvhw"
const ADA_PASTED = "pool18a992crtw6qce9az4kuv8nkeunhl5pgsrvnrz0z82fwksjtpwtj"
const ADA_UNREGISTERED = "pool1t3nhylvgjw02nd9let27p6lkqyxpwg3d8pp5uktydaag2lj5t83"
const BRAM_MISMATCHED = "pool10xzglx49kzaud5wuule06zqnrc5ng06224sxka5p3jt6yz46g5g"
const BRAM_CONTRADICTED = "pool1j6s6ed7zehvw8mheqs835ffs8dr9zhr8wf7c3yu74x6t7al9j8x"
const CAI = "pool16rd7du0uqufp62pn8ey4ghm2wkqgh94p4jmu9nwcu0h0jp7w2a2"
const DAI = "pool102xtd26h9r7dw068zdc5645r736793lujx7fpgcfmajhyxs7ny8"
const DAI_CODE = "f4636c5753b889dcffe0d004dcc8fcd36368d689e3c9015abb5ddb27289aaa4e"
const STRANGER = "pool1kwlvn4xlat6sqzckyykrwsjdtp3ku7vy37d2tv9mcmgacf7uce4"

def fixture [file: path]: nothing -> string {
  open --raw $file
}

def applied []: nothing -> table {
  fixture $APPLICATIONS
  | from csv --no-infer
  | normalise --code-column "application_code" --pool-columns ["pool_id_1" "pool_id_2" "pool_id_3"]
}

def checked []: nothing -> table {
  applied | check (fixture $REGISTERED | read-registered)
}

def three-columns-flatten-to-one-row-per-pool [] {
  assert equal (applied | sort-by pool_id) ([
    {pool_id: $ADA_FIRST, application_code: "MUSA-0001"}
    {pool_id: $ADA_PASTED, application_code: "MUSA-0001"}
    {pool_id: $ADA_UNREGISTERED, application_code: "MUSA-0001"}
    {pool_id: $BRAM_MISMATCHED, application_code: "MUSA.0002"}
    {pool_id: $BRAM_CONTRADICTED, application_code: "MUSA.0002"}
    {pool_id: $CAI, application_code: "MUSA_0003"}
    {pool_id: $DAI, application_code: $DAI_CODE}
  ] | sort-by pool_id)
}

def a-pasted-pool-id-keeps-none-of-its-whitespace [] {
  assert ($" ($ADA_PASTED)" in (fixture $APPLICATIONS))
  assert equal (applied | where pool_id == $ADA_PASTED | length) 1
}

def a-row-naming-no-pool-is-refused [] {
  let rows = [{application_code: "MUSA-0004", pool_id_1: "", pool_id_2: "  "}]
  assert error {|| $rows | normalise --code-column "application_code" --pool-columns ["pool_id_1" "pool_id_2"] | ignore }
}

def a-column-the-header-does-not-have-is-refused [] {
  let rows = [{application_code: "MUSA-0004", pool_id_1: $CAI}]
  assert error {|| $rows | normalise --code-column "application_code" --pool-columns ["pool_id_1" "pool_id_9"] | ignore }
}

def a-pool-that-applied-under-two-codes-is-refused [] {
  let rows = [
    {application_code: "MUSA-0004", pool_id_1: $CAI}
    {application_code: "MUSA-0005", pool_id_1: $CAI}
  ]
  assert error {|| $rows | normalise --code-column "application_code" --pool-columns ["pool_id_1"] | ignore }
}

def a-code-outside-the-identifier-alphabet-is-refused [] {
  let rows = [{application_code: "MUSA 0004", pool_id_1: $CAI}]
  assert error {|| $rows | normalise --code-column "application_code" --pool-columns ["pool_id_1"] | ignore }
}

def the-answers-unreadable-rows-are-dropped [] {
  assert equal (fixture $REGISTERED | read-registered | length) 8
}

def an-answer-missing-a-column-is-refused [] {
  assert error {|| "pool_id\nnot-a-pool\n" | read-registered | ignore }
}

def each-pools-code-is-checked-against-its-registration [] {
  let verdicts = checked | reduce --fold {} {|row, seen| $seen | insert $row.pool_id $row.verdict }
  assert equal $verdicts {
    ($ADA_FIRST): "allowed"
    ($ADA_PASTED): "allowed"
    ($ADA_UNREGISTERED): "not-registered"
    ($BRAM_MISMATCHED): "code-mismatch"
    ($BRAM_CONTRADICTED): "contradictory-codes"
    ($CAI): "allowed"
    ($DAI): "allowed"
  }
}

def a-pool-that-never-applied-is-in-no-verdict [] {
  assert equal (checked | where pool_id == $STRANGER) []
}

def the-emitted-block-is-the-allowlist-the-server-config-reads [] {
  let emitted = checked
    | where verdict == "allowed"
    | select pool_id application_code
    | to-toml
  assert equal ($emitted | from toml) {
    ingest: {
      allowlist: {
        ($ADA_FIRST): "MUSA-0001"
        ($ADA_PASTED): "MUSA-0001"
        ($CAI): "MUSA_0003"
        ($DAI): $DAI_CODE
      }
    }
  }
}

def generate [registrations: path]: nothing -> record {
  let run = [
    $GENERATOR "generate" $APPLICATIONS $registrations
    "--code-column" "application_code"
    "--pool-columns" "pool_id_1,pool_id_2,pool_id_3"
  ]
  ^$nu.current-exe ...$run | complete
}

def the-command-emits-what-the-library-does [] {
  let emitted = checked | where verdict == "allowed" | select pool_id application_code | to-toml
  assert equal (generate $REGISTERED | get stdout | str trim --right) ($emitted | str trim --right)
}

def an-allowlist-nobody-is-on-is-refused [] {
  let empty = $nu.temp-dir | path join "metsuke-allowlist-nobody.csv"
  "pool_id,application_code\n" | save --force $empty
  assert not ((generate $empty | get exit_code) == 0)
  rm $empty
}

def main [] {
  three-columns-flatten-to-one-row-per-pool
  a-pasted-pool-id-keeps-none-of-its-whitespace
  a-row-naming-no-pool-is-refused
  a-column-the-header-does-not-have-is-refused
  a-pool-that-applied-under-two-codes-is-refused
  a-code-outside-the-identifier-alphabet-is-refused
  the-answers-unreadable-rows-are-dropped
  an-answer-missing-a-column-is-refused
  each-pools-code-is-checked-against-its-registration
  a-pool-that-never-applied-is-in-no-verdict
  the-emitted-block-is-the-allowlist-the-server-config-reads
  the-command-emits-what-the-library-does
  an-allowlist-nobody-is-on-is-refused
  print "allowlist tests passed"
}
