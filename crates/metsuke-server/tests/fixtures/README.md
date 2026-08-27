# Fixtures

`recordings/s3/` is the cassette the archive suite replays: one file per
exchange, captured by scripts/record-s3-fixtures.sh from a single-node Garage
on loopback. Each file holds the request that drew the answer as a `#` comment,
then the answer as it arrived — status line, headers, a blank line, the body
verbatim. `support::Reply` writes and reads that format; nothing else parses
one. Never edit a recording by hand.

The requests are the production `S3Archive`'s own, presigned and sent through
a proxy that writes down what came back, so a recorded 2xx is evidence that a
real endpoint accepted what this server signs. The exceptions are the paged
listings and the two objects written without going through `store`: what those
answered is still the endpoint's, and the request line in each file says what
it was asked.

Worth knowing beyond what the filenames say:

- `get-unadorned` — an object with no metadata beside it, which only something
  other than this server writes.
- `put-refused` — a presigned PUT from a key the bucket grants nothing.
- `list-page-1..3` — the same corpus walked by continuation token, bounded to
  a key a page so that the walk has pages at all. What is recorded is the
  answer; nothing in it depends on which bound produced it.
- `list-after` — one bounded page from a cursor, which S3 reads exclusively.

Re-record on a Garage bump, and commit the new bodies with whatever the tests
had to change to keep passing. The suite reads the keys and the tokens out of
these files, so a re-recording moves no expected value by hand.
