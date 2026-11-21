# Threat model

Every peer is untrusted. The operator is trusted for availability and correct accounting, but
**not** for confidentiality.

## Assets, in priority order

1. Confidentiality and integrity of user data
2. Durability: no data loss while the policy is met
3. Fairness: quota and bandwidth rules
4. Availability
5. Metadata privacy (limited, see below)

## Actors

### Malicious client

| Attack | Defence | Residual risk |
|---|---|---|
| Patch client to skip quota | Hosts require control-plane grants; usage is counted from committed pieces | None known |
| Forge, reuse or stretch a grant | PASETO signature, single-use nonce, expiry, and binding to host, pool, peer and blob | Nonce store must survive restart |
| Download beyond entitlement via many hosts | Token bucket at grant issue covers the aggregate | Limit is coarse by one grant |
| Withhold read receipts | Reader was already debited at grant | Host loses credit for up to one grant: a whole piece, or a range for partial reads. Readers who habitually withhold aren't detected yet |
| Use a grant for one blob to fetch others, through a hash-sequence request | Hosts serve only single-blob requests (`v04_hash_sequence_requests_cannot_reach_other_blobs`) | None known. Found while designing range grants |
| Stretch a range grant | Hosts check requested chunks are within the granted range | None known |
| Delete everything with a stolen phrase (`forget` then `prune`) | None yet: deletion is immediate | **Open. A delete grace window is designed but not built** (`plans/reads-reclaim-repair.md`, PR 5) |
| Store plaintext using a patched client | Can't be enforced remotely | Only exposes the attacker's own data, and hosts may hold it unknowingly |
| Use the network to host illegal content | Takedown by segment ID, invite accountability | **Open legal issue. Operator can't inspect content** |

### Malicious host

| Attack | Defence | Residual risk |
|---|---|---|
| Read stored data | Restic's encryption (AES-256-CTR with Poly1305), keys never leave owner | Traffic and pack sizes |
| Corrupt or discard data | Audits, reader verification, repair, disqualification | Small losses take time to detect (3,000 audits for 0.1%) |
| Serve bad data | BLAKE3 verified streaming, then restic's own checks; client tries another piece | Wasted bandwidth |
| Fetch from a peer when audited | Audit timeouts | **Not fully prevented**, and an audit that times out isn't counted as a failure |
| Pledge false capacity | Bounded credit headroom, placement testing | Temporary credit up to `headroom` × held plus allowance |
| Throttle after probes | Real-transfer receipts cross-check probes, reputation | Small-scale gaming possible |
| Go offline after uploading own data | Over-quota lifecycle deletes the leaver's data | Network pays one repair |

### Sybil operator

| Attack | Defence | Residual risk |
|---|---|---|
| Hold all replicas of a segment | Control-plane random placement, per-site limit, invites | About `f³` of segments for attacker fraction `f` |
| Keep one copy, claim three | EC pieces differ already. For `k=1`, only random placement and the per-site limit | **Accepted.** Whitening was removed as a custom construction. An operator holding several replicas of a segment can keep one. Saved disk is roughly its share of the pool, and those copies aren't really independent. Prefer EC in open pools |
| Farm allowances with fake accounts | Invite accountability; allowance is small | **Open signup needs stronger identity** |
| Flood probes or audits | Assigned by control plane, signed, median | Colluding probers need a majority of a random sample |

**Slander.** Three colluding accounts can report refusals against any host and get it disqualified. The
threshold stops a lone client, not a group. Before public signup, require the control plane to confirm a refusal
itself, for example with a test write, rather than trusting reports.

### Colluding client and host

| Attack | Defence | Residual risk |
|---|---|---|
| Fabricate served bytes for credit | Reader debited at grant; zero-sum | None known |
| Store data outside the system | Earns no credit, not tracked | Not our concern |
| Bias placement | Client never chooses hosts | None known |

### Compromised control plane or insider

| Attack | Defence | Residual risk |
|---|---|---|
| Read user data | Holds no keys | None if client is honest |
| Hide or roll back snapshots | Contents can't be forged, since restic names files by hash. A client remembers the snapshots it has seen | A new device has no record, and an older state can look complete to it |
| Delete or misplace data | None technically | **Availability and durability loss.** Backups and audit logs required |
| Forge grants | Hosts trust its key; damage limited to availability | Key rotation is future work |
| Ship a malicious client | Open source, reproducible builds | **Future work** |

### Network observer

Peer traffic is encrypted by `iroh`. The control-plane API has no TLS of its own, so it needs a proxy in front. Traffic timing
and volume stay visible.

## What leaks

| Observer | Sees | Doesn't see |
|---|---|---|
| Control plane | Account, pool, repository object types, IDs and sizes, snapshot counts, access times, IP addresses | File names, contents, keys |
| Host | Client IP and endpoint, piece hash and size, access times | Owner identity beyond the grant's account ID, contents |

Grants carry an account ID, so hosts can link pieces to accounts. An opaque,
per-pool token would reduce this and should be evaluated.

Hosts can tell when they hold the same replica, since `k=1` copies share one hash.

## Accepted for the prototype

- Control-plane API without built-in TLS
- No key rotation
- Application-level link shaping where `tc` isn't available
- Tested on loopback only: hole punching and relays are `iroh`'s, and haven't been exercised here
- Simple single-device key handling
- Self-declared site and ASN. Production must derive these from observed connection addresses,
  because otherwise the per-site Sybil rule can be evaded by lying
- Node owner seed and node identity stored unencrypted on disk
