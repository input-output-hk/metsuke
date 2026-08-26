# Wire recordings

`v1-envelope.hex` is one sealed envelope's wire bytes, lowercase hex on one
line. It was produced by a build that knew only schema v1, so opening it is the
evidence that a v2-capable build still reads what v1 agents shipped and what the
archive already holds (ADR 0005). The signature is not recorded: Ed25519 is
deterministic, so the test re-signs the same bytes with the same key.

v1 recorded from 274aace40e2976853deacf5bac6046ebc3e3aac8

Re-record with scripts/record-v1-envelope.sh, which compiles that revision's own
`envelope.rs` in a scratch crate. Never edit by hand: a hand-set byte says
nothing about what any build produced.
