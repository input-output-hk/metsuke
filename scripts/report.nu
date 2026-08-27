#!/usr/bin/env nu

# Runs a test command whole, keeps its entire output on disk, and prints a few
# lines about it. Nothing here caps the command's output: a run whose
# interesting line was in the part `| tail` threw away is a run that has to
# happen again.
#
# A passing run is three lines, because the only things anyone acts on are the
# count, what was slow, and where to look. A failing run prints every failure
# and what it asserted. The full breakdown is `--table`; the full output is the
# log, whose path is always the last thing printed.

const LOG_DIR = ".test-logs"

# Where a label's output lands. One stable name per label rather than a
# timestamp, so a reader who was not there can still find the last run.
def log-path [label: string]: nothing -> string {
  mkdir $LOG_DIR
  [$LOG_DIR $"($label).log"] | path join
}

def secs [span: duration]: nothing -> float {
  $span / 1sec | math round --precision 2
}

# Run `command` with its whole output redirected to the label's log. The exit
# status comes back rather than raising: the log is what says why it failed, and
# every caller reports before it exits.
def capture [
  label: string
  command: string
  args: list<string>
]: nothing -> record<log: string, ok: bool, took: duration> {
  let log = log-path $label
  let started = date now
  # `o+e>` keeps stderr interleaved with stdout: cargo writes the `Running`
  # lines to stderr and the test lines to stdout, and their order is what pairs
  # a result with the binary that produced it.
  let ok = try {
    ^$command ...$args o+e> $log
    true
  } catch {
    false
  }
  { log: $log, ok: $ok, took: ((date now) - $started) }
}

# The slowest few, named inline. What a table would add on a passing run is
# rows nobody reads.
def slowest [rows: table, name: string, count: int]: nothing -> string {
  $rows
    | sort-by secs --reverse
    | first $count
    | each {|row| $"($row | get $name) ($row.secs)s" }
    | str join ", "
}

# The tail of a log, for a failure whose shape this script does not parse.
def show-tail [log: string, count: int] {
  print $"  last ($count) lines:"
  open --raw $log | lines | last $count | each {|line| print $"    ($line)" }
}

# ---------------------------------------------------------------- cargo test

# `Running tests/foo.rs (…/deps/foo-4f92e547080e9c9f)` → the target plus a
# short id, because two crates in this workspace both have a `tests/binary.rs`
# and the deps hash is the only thing that tells their results apart.
def cargo-target [line: string]: nothing -> string {
  let parsed = $line | str trim | parse -r '^Running (?<target>.+?) \(.*/deps/[^-]+-(?<hash>[0-9a-f]+)\)$'
  if ($parsed | is-empty) {
    return "?"
  }
  $parsed | first | get target
}

# Pair each `test result:` with the `Running` line above it. Walked by index
# rather than accumulated in the loop, because a per-iteration `append` is
# quadratic and this runs over every line of the log.
def cargo-rows [lines: list<string>]: nothing -> table {
  let running = $lines
    | enumerate
    | where ($it.item | str trim | str starts-with "Running ")
  let results = $lines
    | enumerate
    | where ($it.item | str starts-with "test result:")

  $results | each {|result|
    let owner = $running | where index < $result.index | last
    let counts = $result.item
      | parse -r 'test result: (?<verdict>\w+)\..*?(?<passed>\d+) passed; (?<failed>\d+) failed; (?<ignored>\d+) ignored.*finished in (?<secs>[0-9.]+)s'
    if ($counts | is-empty) {
      return null
    }
    let row = $counts | first
    {
      secs: ($row.secs | into float)
      target: (if $owner == null { "?" } else { cargo-target $owner.item })
      passed: ($row.passed | into int)
      failed: ($row.failed | into int)
      ignored: ($row.ignored | into int)
    }
  } | compact
}

# The tests that failed and what each one asserted. The counts do not name
# them, and the assertion is the whole reason anyone reads a failing run — so it
# goes in the summary rather than behind a second command.
def cargo-failures [lines: list<string>]: nothing -> table {
  let names = $lines
    | where ($it | str starts-with "test ") and ($it | str ends-with " ... FAILED")
    | each {|line| $line | str replace "test " "" | str replace " ... FAILED" "" }

  $names | uniq | each {|name|
    # libtest writes each failure's captured output under this heading, so the
    # panic that follows the heading is that test's own.
    let start = $lines
      | enumerate
      | where ($it.item | str starts-with $"---- ($name) ")
      | get --optional 0.index
    let detail = if $start == null {
      []
    } else {
      $lines
        | skip $start
        | take 12
        | where ($it | str contains "panicked at") or ($it | str starts-with "assertion") or ($it | str starts-with "  left:") or ($it | str starts-with " right:")
    }
    { test: $name, detail: $detail }
  }
}

def report-cargo [run: record, table_view: bool]: nothing -> int {
  let lines = open --raw $run.log | lines
  let rows = cargo-rows $lines

  # No test result anywhere means nothing ran — a compile error, a panic in a
  # build script, cargo refusing the arguments. Reporting the counts here would
  # print a green zero.
  if ($rows | is-empty) {
    print "BUILD FAILED — no test ran"
    let errors = $lines | where ($it | str starts-with "error")
    if ($errors | is-not-empty) {
      $errors | each {|line| print $"  ($line)" }
    } else {
      show-tail $run.log 30
    }
    print $"  log: ($run.log)"
    return 1
  }

  let failures = cargo-failures $lines
  let totals = {
    passed: ($rows | get passed | math sum)
    failed: ($rows | get failed | math sum)
    ignored: ($rows | get ignored | math sum)
  }

  if ($failures | is-not-empty) or ($totals.failed > 0) {
    print $"FAILED — ($totals.failed) failed, ($totals.passed) passed"
    $failures | each {|failure|
      print $"  ($failure.test)"
      $failure.detail | each {|line| print $"      ($line | str trim)" }
    }
    print $"  log: ($run.log)"
    return 1
  }

  if $table_view {
    $rows | sort-by secs --reverse | print
  }
  print $"ok — ($totals.passed) passed, ($totals.ignored) ignored, ($rows | length) binaries in (secs $run.took)s"
  print $"  slowest: (slowest $rows target 3)"
  print $"  log: ($run.log)"
  # A suite that ran but whose runner still failed is a runner problem, not a
  # test one, and it must not read as green.
  if not $run.ok { return 1 }
  0
}

# --------------------------------------------------------------- nixos tests

# Replace a build log with the VM's own output, read back out of the store.
# A cached build prints nothing, so parsing what `nix build` said would report a
# passing test as an empty one; `nix log` answers the same either way.
def vm-output [run: record]: nothing -> record {
  let built = open --raw $run.log
    | lines
    | where ($it | str starts-with "/nix/store/")
    | last
  if $built == null {
    return $run
  }
  try {
    ^nix log $built o+e> $run.log
  } catch {
    # The build result is what matters; a log nix cannot produce is reported
    # below as a run with no subtests in it.
  }
  $run
}

# The driver prints `(finished: subtest: NAME, in N seconds)` per subtest and
# one line for the script as a whole.
def report-vm [run: record, table_view: bool]: nothing -> int {
  let lines = open --raw $run.log | lines
  let subtests = $lines
    | parse -r '\(finished: subtest: (?<subtest>.+?), in (?<secs>[0-9.]+) seconds\)'
    | each {|row| { secs: ($row.secs | into float), subtest: $row.subtest } }
  let whole = $lines
    | parse -r '\(finished: run the VM test script, in (?<secs>[0-9.]+) seconds\)'

  if not $run.ok {
    print "VM TEST FAILED"
    # The driver's own error, which is the line that names the failing assert.
    let errors = $lines | where ($it =~ 'Traceback|error:|AssertionError|Test failed')
    if ($errors | is-not-empty) {
      $errors | last 15 | each {|line| print $"  ($line)" }
    } else {
      show-tail $run.log 30
    }
    print $"  log: ($run.log)"
    return 1
  }

  if $table_view and ($subtests | is-not-empty) {
    $subtests | sort-by secs --reverse | print
  }
  let script = if ($whole | is-empty) { "?" } else { $"($whole | first | get secs)s" }
  print $"ok — ($subtests | length) subtests, script ($script)"
  if ($subtests | is-not-empty) {
    print $"  slowest: (slowest $subtests subtest 2)"
  }
  print $"  log: ($run.log)"
  0
}

# ------------------------------------------------------------- flake check

def report-check [run: record]: nothing -> int {
  let lines = open --raw $run.log | lines
  let checked = $lines | parse -r 'checking derivation (?<check>checks\.[^\s.]+\.(?<name>\S+))\.\.\.'

  if not $run.ok {
    print "FLAKE CHECK FAILED"
    $lines
      | where ($it =~ 'error:|failed with exit code|^\s+> ')
      | last 25
      | each {|line| print $"  ($line)" }
    print $"  log: ($run.log)"
    return 1
  }

  # The names of the checks that passed are not something anyone acts on.
  print $"ok — ($checked | length) derivations checked in (secs $run.took)s"
  print $"  log: ($run.log)"
  0
}

# ------------------------------------------------------------------ recipes

def main [] {
  print "run through the justfile: `just --list`"
}

# cargo test over the workspace, or whatever filter is passed through.
# `--no-fail-fast`: without it cargo stops at the first failing binary and the
# summary counts only what ran, which reads as a smaller failure than it is.
def "main cargo" [--table, ...args: string] {
  let run = capture "cargo-test" "cargo" ([test --workspace --no-fail-fast] ++ $args)
  exit (report-cargo $run $table)
}

# Per-test wall time inside one test binary, which the suite summary cannot
# give: libtest reports per-binary, and `--report-time` needs nightly.
#
# The binary is resolved through cargo rather than taken as a path, because a
# path found by hand is the stale artefact of an earlier build as easily as the
# current one — and a stale binary reports failures that are not there.
def "main slow" [
  target: string # a test target's name, e.g. `envelope` or `binary`
  --package (-p): string = "" # which crate's, where a target name is in both
  --top: int = 15
] {
  let artifacts = ^cargo test --workspace --no-run --message-format json
    | lines
    | each {|line| try { $line | from json } }
    | compact
    | where reason == "compiler-artifact" and ($it.target.kind | any {|k| $k == "test" })
    | where executable != null
    | where ($package | is-empty) or ($it.package_id | str contains $"/($package)#")

  # A mistyped target and an ambiguous one are both the caller's to fix, so they
  # get a message and an exit code rather than a span into this script.
  let matched = $artifacts | where ($it.target.name | str contains $target)
  if ($matched | is-empty) {
    print --stderr $"no test target matches '($target)'. available:"
    print --stderr ($artifacts | get target.name | uniq | sort | str join ", ")
    exit 1
  }
  if ($matched | length) > 1 {
    # Two crates here both have a `binary` target, so the ambiguity is real and
    # picking one silently would time whichever cargo listed first.
    print --stderr $"'($target)' is a target in more than one crate — add -p:"
    $matched | each {|a|
      let crate = $a.package_id | parse -r '/(?<crate>[^/#]+)#' | get --optional 0.crate
      print --stderr $"  -p ($crate)"
    }
    exit 1
  }

  let binary = $matched | first | get executable
  let names = ^$binary --list --format terse
    | lines
    | where ($it | str ends-with ": test")
    | each {|line| $line | str replace ": test" "" }

  # Wall time per process, so each row carries one spawn's overhead (~5-10ms).
  # Only the ordering and the large values are meant to be read.
  $names
    | each {|name|
      let started = date now
      try { ^$binary --exact $name --test-threads=1 o+e> /dev/null }
      { secs: (((date now) - $started) / 1sec | math round --precision 3), test: $name }
    }
    | sort-by secs --reverse
    | first $top
    | print
}

# The NixOS unit test (hydraJobs.units).
def "main vm" [--table] {
  let run = capture "nix-units" "nix" [
    build ".#hydraJobs.units.x86_64-linux" -L --no-link --print-out-paths
  ]
  exit (report-vm (vm-output $run) $table)
}

# The end-to-end test, which lives in devnet's flake because the node is pinned
# there.
def "main e2e" [--table] {
  let run = capture "nix-e2e" "nix" [
    build "path:devnet#hydraJobs.e2e.x86_64-linux" -L --no-link --print-out-paths
  ]
  exit (report-vm (vm-output $run) $table)
}

# clippy, treefmt, audit, deny and the sandboxed test builds.
def "main check" [] {
  let run = capture "nix-check" "nix" [flake check -L]
  exit (report-check $run)
}
