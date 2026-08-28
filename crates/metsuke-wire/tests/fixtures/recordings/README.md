# Wire recordings

`submission-scrapes.hex` and `submission-lines.hex` are one sealed submission
of each payload shape, lowercase hex on one line. They pin the container's
framing: what `seal` writes into the skippable frame and the data frame, and
therefore what a bucket holds and what `zstd -d` hands a tool that never heard
of metsuke. The signature is not recorded because Ed25519 is deterministic, so
the tests re-sign the same bytes with the same key.

Re-record with `scripts/record-submission.sh`. The values sealed live in
`crates/metsuke-wire/examples/record-submission.rs`, which calls the same
`seal` the agent does. Never edit by hand: a hand-set byte says nothing about
what any build produced.
