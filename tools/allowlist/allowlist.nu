#!/usr/bin/env nu

# The application-code gate, run once when the server configuration is
# generated. `query` reads the chain half off a db-sync; `generate` is offline
# and pure, so the answer it was checked against is a file that can be kept.

const POOL_ID = '^pool1[qpzry9x8gf2tvdw0s3jn54khce6mua7l]{51}$'
const CODE = '^[A-Za-z0-9._-]+$'
const POOL_ID_COLUMN = "pool_id"
const CODE_COLUMN = "application_code"

# An earlier update's code is one the operator has already replaced, so only the
# current registration counts. DISTINCT because one transaction may carry
# several registration certificates for a pool.
const REGISTERED_CODES = "
SELECT DISTINCT ph.view AS pool_id,
       tm.json ->> :'code_key' AS application_code
FROM pool_hash ph
JOIN pool_update pu ON pu.hash_id = ph.id
JOIN tx_metadata tm ON tm.tx_id = pu.registered_tx_id
WHERE tm.key = :label
  AND tm.json ? :'code_key'
  AND pu.registered_tx_id = (
        SELECT MAX(registered_tx_id) FROM pool_update WHERE hash_id = ph.id)
  AND NOT EXISTS (
        SELECT 1
        FROM pool_retire pr
        WHERE pr.hash_id = ph.id
          AND pr.announced_tx_id > pu.registered_tx_id)
"

def demand [value: any, name: string] {
  if $value == null {
    error make {msg: $"($name) is required"}
  }
  $value
}

def field [row: record, name: string, at: int] {
  let value = $row | get --optional $name
  if $value == null {
    error make {msg: $"row ($at): no ($name) column, found (($row | columns) | str join ', ')"}
  }
  $value | str trim
}

# One row per applicant becomes one row per pool: the columns are optional and
# what an operator pasted into a form carries its whitespace.
export def normalise [
  --code-column: string
  --pool-columns: list<string>
]: table -> table<pool_id: string, application_code: string> {
  let code_column = demand $code_column "--code-column"
  let pool_columns = demand $pool_columns "--pool-columns"
  # Bound rather than piped on: an error raised inside a streaming `each` is
  # carried as a value through `sort-by` instead of stopping the run.
  let pools = $in
  | enumerate
  | each {|entry|
      let at = $entry.index + 2
      let code = field $entry.item $code_column $at
      if not ($code =~ $CODE) {
        error make {msg: $"row ($at): ($code) is not an application code"}
      }
      let pools = $pool_columns
        | each {|name| field $entry.item $name $at }
        | where {|found| $found != "" }
      if ($pools | is-empty) {
        error make {msg: $"row ($at): none of ($pool_columns | str join ', ') names a pool"}
      }
      $pools | each {|pool_id|
        if not ($pool_id =~ $POOL_ID) {
          error make {msg: $"row ($at): ($pool_id) is not a pool id"}
        }
        {pool_id: $pool_id, application_code: $code}
      }
    }
  | flatten
  | group-by --to-table pool_id
  | insert codes {|group| $group.items | get application_code | uniq }
  let clashing = $pools | where {|group| ($group.codes | length) > 1 }
  if ($clashing | is-not-empty) {
    let told = $clashing | each {|group| $"($group.pool_id) as ($group.codes | str join ', ')" }
    error make {msg: $"a pool applied under more than one code: ($told | str join '; ')"}
  }
  $pools
  | each {|group| {pool_id: $group.pool_id, application_code: ($group.codes | first)} }
  | sort-by pool_id
}

# Any pool operator on the chain can register the label under any value, so a
# row that does not read is dropped rather than refused: it matches no
# application either way, and failing on it would let a stranger's transaction
# stop every onboarding.
export def read-registered []: string -> table<pool_id: string, application_code: string> {
  let rows = $in | from csv --no-infer --trim all
  let missing = [$POOL_ID_COLUMN $CODE_COLUMN] | where {|name| $name not-in ($rows | columns) }
  if ($missing | is-not-empty) {
    error make {msg: $"the answer has no ($missing | str join ', ') column"}
  }
  $rows
  | select $POOL_ID_COLUMN $CODE_COLUMN
  | rename pool_id application_code
  | where {|row| ($row.pool_id =~ $POOL_ID) and ($row.application_code =~ $CODE) }
}

export def check [
  registered: table<pool_id: string, application_code: string>
]: table -> table<pool_id: string, application_code: string, verdict: string, registered_code: string> {
  let by_pool = $registered | group-by pool_id
  $in | each {|applied|
    let codes = $by_pool
      | get --optional $applied.pool_id
      | default []
      | get application_code
      | uniq
    let verdict = match ($codes | length) {
      0 => "not-registered"
      1 => (if ($codes | first) == $applied.application_code { "allowed" } else { "code-mismatch" })
      _ => "contradictory-codes"
    }
    {
      pool_id: $applied.pool_id
      application_code: $applied.application_code
      verdict: $verdict
      registered_code: ($codes | str join " ")
    }
  }
}

export def to-toml []: table<pool_id: string, application_code: string> -> string {
  let allowlist = $in
    | sort-by pool_id
    | reduce --fold {} {|row, acc| $acc | insert $row.pool_id $row.application_code }
  {ingest: {allowlist: $allowlist}} | to toml
}

def main [] {
  error make {msg: "run `generate` or `query`"}
}

def "main generate" [
  applications: path
  registrations: path
  --code-column: string
  # Comma separated, in the order the export has them.
  --pool-columns: string
]: nothing -> string {
  let named = demand $pool_columns "--pool-columns" | split row "," | each {|name| $name | str trim }
  let checked = open --raw $applications
    | from csv --no-infer
    | normalise --code-column (demand $code_column "--code-column") --pool-columns $named
    | check (open --raw $registrations | read-registered)
  let allowed = $checked | where verdict == "allowed"
  for excluded in ($checked | where verdict != "allowed") {
    print --stderr $"($excluded.pool_id): ($excluded.verdict), applied ($excluded.application_code), registered ($excluded.registered_code)"
  }
  # An allowlist nobody is on stops every upload, and prints exactly like a
  # program nobody joined.
  if ($allowed | is-empty) {
    error make {msg: $"no pool passed the gate, of ($checked | length) that applied"}
  }
  $allowed | select pool_id application_code | to-toml
}

def "main query" [
  --socket-dir: path
  --dbname: string
  --role: string
  --metadata-label: int
  --metadata-key: string
  --statement-timeout: duration
]: nothing -> string {
  let timeout_ms = ((demand $statement_timeout "--statement-timeout") / 1ms) | into int
  let arguments = [
    "--host" (demand $socket_dir "--socket-dir")
    "--dbname" (demand $dbname "--dbname")
    "--username" (demand $role "--role")
    "--no-password" "--csv" "--no-psqlrc"
    "--variable" "ON_ERROR_STOP=1"
    "--variable" $"code_key=(demand $metadata_key '--metadata-key')"
    "--variable" $"label=(demand $metadata_label '--metadata-label')"
    "--file" "-"
  ]
  let answer = with-env {PGOPTIONS: $"-c statement_timeout=($timeout_ms)"} {
    $REGISTERED_CODES | ^psql ...$arguments | complete
  }
  if $answer.exit_code != 0 {
    error make {msg: $"psql failed: ($answer.stderr)"}
  }
  $answer.stdout
}
