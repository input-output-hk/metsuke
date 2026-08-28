# Architecture decision records

An ADR is an accepted decision and the reasoning that produced it. It is not a
description of the code. The code is described by the code.

## Editing one after acceptance

The claims are frozen. The words are not.

So fix the words freely, anywhere in the file: a banned term, punctuation,
wrapping, a sentence that needs splitting, a name for something that no longer
exists. A list of what a rule covered when it was written is the same case.
Restate the rule and drop the list.

Do not rewrite a claim so the decision reads better in hindsight, and do not add
a consequence nobody foresaw. Both belong in the ADR that supersedes this one.

A decision that no longer holds is superseded, never edited and never deleted.
Write the new ADR, then amend the old one's status. 0007 is the worked example.
Numbers are never reused; the gaps are ADRs deleted before this rule existed.

## Writing one

`Status`, then `Context`, `Decision`, `Consequences`. Be blunt. An ADR that
hedges has not decided anything.

State a rule, not the things it happens to cover today. An enumeration is what
goes stale.

State the consequence you did not like alongside the one you did. The decisions
worth recording are the ones with a real cost, and the cost is what a reader
needs before they try to undo it.
