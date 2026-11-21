# Protocol

Types live in `crates/proto`. This documents behaviour, rather than freezing a wire format.

## Connections

| Between | Transport | Purpose |
|---|---|---|
| Client or node, and control plane | HTTP with JSON | The API. Needs a TLS proxy in front in production |
| Peers | `iroh` QUIC | Encrypted and authenticated by endpoint key |

Peers use two protocols on the same endpoint:

- `iroh-blobs`: bytes, as BLAKE3 verified streams
- `globalobject/peer/1`: short JSON requests to authorise and coordinate transfers

## Tokens

All tokens are PASETO v4.public, with a purpose string as the implicit assertion, so one kind can't stand in for another.

| Token | Signed by | Holds |
|---|---|---|
| Grant | Control plane | `nonce, account, pool, segment, piece, host, peer, hash, op, max_bytes, rate_bps, expires_at`, and optional `range` (bytes of the piece) |
| Store receipt | Host | `nonce, segment, piece, host, bytes, hash` |
| Read receipt | Reader | `nonce, host, reader, bytes` |
| Request | Account or node | `id, method, path, ts, body hash`, rejected outside a 5-minute window |

A grant works only for the host, peer, blob and operation it names, and only once. A grant with a `range` covers only those bytes.

## Peer requests

| Request | Effect |
|---|---|
| `Authorize { grant }` | Allows the caller one fetch of that blob, or of the granted range of it, paced at the grant's rate |
| `Store { grant, from }` | The host fetches the piece from `from`, verifying it and refusing more than `max_bytes`, then returns a store receipt |
| `Receipt { reader, nonce, token }` | A read receipt for the host to pass on to the control plane |

Hosts refuse `iroh-blobs` requests unless authorised, and refuse "observe" and "push" outright. They serve only requests for
the granted blob itself. A request for the children of a hash sequence is refused, because the provider would otherwise serve
the blobs named inside the granted one. A ranged grant is served only if the requested chunks are within it.

## Control-plane API

| Request | Purpose |
|---|---|
| `POST /v1/pools`, `POST /v1/pools/{id}/invites` | Create a pool (operator token), invite |
| `PATCH /v1/pools/{id}` | Change a private pool's policy (admin) |
| `POST /v1/pools/{id}/members/{id}/remove` | Remove a member (admin) |
| `POST /v1/accounts`, `GET /v1/accounts/me` | Register with an invite, show quota and entitlement |
| `POST /v1/nodes`, `PATCH /v1/nodes/{id}`, `POST /v1/nodes/{id}/heartbeat` | Register, change pledge, heartbeat. A heartbeat's reply carries tasks |
| `POST /v1/segments/begin` | Quota check, placement, write grants |
| `POST /v1/segments/{id}/commit`, `/abort` | Verify receipts and publish, or give up |
| `POST /v1/segments/{id}/read` | Token bucket check, read grants |
| `GET /v1/objects/{type}`, `GET /v1/objects/{type}/{id}`, `POST .../delete` | The repository catalogue |
| `POST /v1/receipts/read` | Host reports read receipts |
| `POST /v1/tasks/repair-done`, `/probe-result` | Host reports task results |

Errors: `402` quota exceeded, `403` read-only or removed, `409` the pool's policy changed, `429` bandwidth limit with
`retry_after`, `503` too few failure domains (writes) or nothing reachable (reads).

## Tasks

| Task | Node action |
|---|---|
| `Repair` | Authorise at `k` sources, fetch and verify, rebuild the piece, check its hash, keep it, report |
| `Probe` | Fetch the target's probe blob and report the speed |
| `Delete` | Remove a piece's tag. Garbage collection reclaims the space |

Drain is repair onto other hosts, then deletion from the draining host.

## Audits

The auditor authorises itself with a grant, and requests a random 16 KiB range of the piece. The response is a BLAKE3 verified
stream, checked against the piece's hash, which is all the auditor needs to hold. Failures add up, and passes on other pieces
don't cancel them.

## Repository mapping

Each file in the restic-format repository becomes one object in the catalogue, stored as one segment. Packs are limited
to 48 MiB, and segments to 64 MiB.
