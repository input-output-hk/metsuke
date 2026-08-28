# Test recipes. Each one runs its command whole, keeps the entire output under
# .test-logs/, and prints a summary of it, so a run stays searchable afterwards
# instead of being truncated by a `| tail` nobody can undo. The path is printed
# every time; grep it when the summary is not enough.
#
# A passing run is three lines: the count, what was slow, and the log path. Pass
# `--table` to `test` or `vm` for the per-binary breakdown; grep the log for
# anything else.
#
# scripts/report.nu owns the parsing and the exit codes. Only the last line of a
# recipe's comment reaches `just --list`, so each one below is a single line.

_default:
    @just --list

# The Rust suite, every binary even past a failure (~4s).
test *ARGS:
    @nu scripts/report.nu cargo {{ ARGS }}

# Per-test wall time inside one test binary, by target name (e.g. `envelope`).
slow TARGET *ARGS:
    @nu scripts/report.nu slow {{ TARGET }} {{ ARGS }}

# Both units under their confinement, in a VM (~18s, wants /dev/kvm).
vm:
    @nu scripts/report.nu vm

# End to end in a VM: node, bucket, server, two agents (~47s, wants /dev/kvm).
e2e:
    @nu scripts/report.nu e2e

# clippy, treefmt, audit, deny and the sandboxed test builds.
check:
    @nu scripts/report.nu check

# What a commit should pass, cheapest first so a failure comes back fast.
all: test check vm

# Coverage, which is a floor: binary-spawning tests record nothing.
cov:
    cargo llvm-cov --workspace

# What the last runs left behind.
logs:
    @ls -la .test-logs 2>/dev/null || echo "no runs captured yet"
