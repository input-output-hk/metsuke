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
A **Pool**'s long-lived identity key, which an **Operator** may be unwilling to
keep on the telemetry machine.
_Avoid_: pool key, node key

**Calidus Key**:
A short-lived key a **Pool** authorises to speak on its behalf, so the **Cold
Key** need not leave the **Operator**'s safe keeping.
_Avoid_: hot key, session key, delegated key

**Registration**:
An **Operator**'s on-chain declaration that a **Calidus Key** speaks for their
**Pool**, carrying a **Nonce** and proof the **Cold Key** authorised it.
_Avoid_: certificate, record, entry

**Nonce**:
The number that orders a **Pool**'s **Registrations**. The highest is current.
_Avoid_: version, sequence, timestamp

**Revocation**:
A **Registration** that names no **Calidus Key**, withdrawing the **Pool**'s
authorisation without replacing it.

**Witness**:
The **Cold Key**'s signature inside a **Registration**, which is the only thing
distinguishing an **Operator**'s **Registration** from anyone else's claim about
their **Pool**.

### Reporting

**Submission**:
One report a **Pool** sends the server, signed by either its **Cold Key** or its
**Calidus Key**.
_Avoid_: upload, batch, post, payload

**Allowlist**:
The set of **Pools** the rewards program accepts **Submissions** from. Separate
from, and prior to, whether a key speaks for a **Pool**.
_Avoid_: whitelist, roster, participants

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

- A **Pool** has exactly one **Cold Key** and at most one current **Calidus Key**
- A **Pool** may have many **Registrations**; the highest **Nonce** names the current **Calidus Key**
- A **Registration** is authorised by exactly one **Witness** from the **Pool**'s **Cold Key**
- A **Submission** is signed by one key, which must speak for the **Pool** it names
- A **Pool** outside the **Allowlist** has no accepted **Submissions**, whatever key it holds
- A **Pool** reaches the **Allowlist** only when its **Application Code** appears in both an
  **Application** and its current pool registration

## Example dialogue

> **Dev:** "If an **Operator** rotates their **Calidus Key**, when does the old
> one stop working?"
>
> **Domain expert:** "The moment a **Registration** with a higher **Nonce** is on
> chain. There is no expiry on the key itself — a later **Registration** is what
> ends the earlier one, and a **Revocation** ends it without naming a
> replacement."
>
> **Dev:** "So if we look up the pool and find nothing, that's a pool that never
> registered?"
>
> **Domain expert:** "Or one that revoked. Those are different situations for the
> **Operator** and they should not read the same to us."

## Flagged ambiguities

- **Registration** is overloaded in the wider Cardano domain: a pool registration
  certificate is a different thing, and neither is a UTxO. In this context
  **Registration** always means the **Calidus Key** declaration.
- "upload" and "submission" are both used throughout the code for the same thing.
  **Submission** is canonical here; the naming in code is not yet aligned.
