# Metsuke

Telemetry for the MusashiNet rewards program: stake pool operators run an agent
that reports on their node, and a server accepts those reports only from pools
that can prove who they are.

## Language

### Pools and keys

**Pool**:
A stake pool taking part in the rewards program.
_Avoid_: node, operator (see **Operator**), SPO

**Operator**:
The person or organisation running a **Pool**.
_Avoid_: SPO, owner, user

**Pool ID**:
The identifier a **Pool** is known by everywhere, on chain and in a **Submission**.

**Cold Key**:
A **Pool**'s long-lived identity key. The **Pool ID** is its hash, so a
**Submission** it signed needs nothing looked up to say whose it is.
_Avoid_: pool key, node key

**Leios Key**:
A **Pool**'s BLS key, registered on chain in its pool registration and held on
the machines that forge and vote. A **Submission** may be signed by it instead
of by the **Cold Key**, and then whose it is has to be looked up in the **Key
Roster**.
_Avoid_: BLS key (the algorithm, not the role), hot key, vote key

**Key Roster**:
Which **Leios Keys** the chain currently registers for each **Pool**, read off
the chain and handed to the server as a file. The server signs nothing and
queries nothing: it believes the roster it was given.
_Avoid_: snapshot (the stake distribution already has that name), keyset, cache

### Reporting

**Agent**:
One machine reporting on behalf of a **Pool**. A **Pool** may run several, each
signing with the same **Cold Key** and naming itself by its **Agent ID**.
_Avoid_: machine, host, relay, node

**Agent ID**:
The name an **Agent** is known by within its **Pool**. Chosen by the
**Operator**, and unique only within the **Pool**.

**Scrape**:
One read of a node's metrics endpoint, and the one line an **Agent** ships for
it: every metric the endpoint returned, plus the **Agent**'s own scrape time and
clock offset. A failed read is a **Scrape** too, carrying no metrics and the
reason it failed.
_Avoid_: sample, snapshot, poll, metrics, measurement

**Submission**:
One report an **Agent** sends the server, signed by its **Pool**'s **Cold Key**
or its **Leios Key**. Which **Pool** it is from is settled by the key that
signed it, never by the report.
_Avoid_: upload, batch, post, payload

**Frame**:
One of the two parts a **Submission** is made of on the wire: a plaintext
header frame naming the **Pool**, the **Agent** and the **Sequence Number**,
then a compressed data frame of the lines themselves. One signature covers
both.
_Avoid_: section, block, chunk

**Sequence Number**:
The count an **Agent** stamps on each **Submission** it sends. Per **Agent**,
and the server neither reads nor checks it. A number is never handed out twice,
so an attempt the server refused spends one and the same lines go out under a
later one. A gap therefore says a **Submission** did not land; it does not say
its **Scrapes** were lost, and it is not a count of what is missing.
_Avoid_: counter, nonce, offset

**Allowlist**:
The set of **Pools** the rewards program accepts **Submissions** from. Separate
from, and prior to, the signature. A **Pool** off it is refused before any
cryptography runs.
_Avoid_: whitelist, roster (see **Key Roster**), participants

**Application**:
An **Operator** asking for their **Pool** to join the rewards program, carrying
an **Application Code**.
_Avoid_: signup, enrolment, request

**Application Code**:
The string an **Operator** puts in both their **Application** and their pool
registration transaction's metadata. The pair is what shows the **Application**
came from whoever holds the **Pool**'s **Cold Key**, since only that key can
sign a pool registration. Public, never a secret.
_Avoid_: token, secret, invite code

**Developer**:
Someone working on the rewards program who reads **Submissions** back out of
the archive. Neither an **Operator** nor a **Pool**, and authenticated as one
shared account rather than per person.
_Avoid_: user, consumer, analyst, client

## Relationships

- A **Pool** has exactly one **Cold Key**, and the **Pool ID** is that key's hash
- A **Pool** has as many **Leios Keys** as the **Key Roster** lists for it, and
  none of them names it
- A **Pool** may report from many **Agents**, each with its own **Agent ID** and
  its own **Sequence Number** run
- A **Submission** is signed by one key, and that key is what names its **Pool**
- A **Submission** carries whole **Scrapes**, one line each, never a metric on its own
- A **Pool** outside the **Allowlist** has no accepted **Submissions**, whatever key it holds
- A **Pool** reaches the **Allowlist** only when its **Application Code** appears in both an
  **Application** and its current pool registration

## Example dialogue

> **Dev:** "Two relays are reporting for one pool. How do we tell their
> **Submissions** apart?"
>
> **Domain expert:** "By **Agent ID**. Both sign with the same **Cold Key**.
> That is the pool's identity and there is only one, so the **Agent ID** is
> what says which machine. Their **Sequence Numbers** run independently."
>
> **Dev:** "So a gap in one relay's numbers means we lost a **Submission**?"
>
> **Domain expert:** "It means one did not land, from that **Agent**. Reading
> it as the pool's would count the other relay's uploads as missing. And a
> refused attempt spends its number and sends its lines under the next, so the
> gap is usually the retry rather than a loss. What was actually collected is
> what the **Scrapes** say they were taken at."

> **Dev:** "If a **Submission** is signed by a **Leios Key**, how do we know
> whose it is? The key does not hash to the **Pool ID**."
>
> **Domain expert:** "It says which **Pool** it claims, and we believe that
> only once the signature stands under a key the **Key Roster** files for that
> pool. A pool cannot claim another's, because it cannot sign for it. What we
> lose is that the answer now depends on the roster: the **Cold Key** settles
> it alone and forever, and a **Leios Key** settles it only against a roster
> that was current."

## Flagged ambiguities

- "upload" and "submission" are both used throughout the code for the same thing.
  **Submission** is canonical here; the naming in code is not yet aligned.
- "BLS key" is what the cli, the node flag and the operators say; the ledger
  says `leiosKey`. **Leios Key** is canonical here, because "BLS" names an
  algorithm and several other things could use one.
