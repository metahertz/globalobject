# Draft issue (then PR) for `n0-computer/iroh-blobs`: get-request hooks only see the root hash

Status: **draft, not sent.** Written against 0.104.1. Raise it privately first (security contact or maintainers), because other
projects may gate access on the root hash the way we did. It's a footgun in the event API, not a bug in `iroh-blobs` itself:
the hash-sequence behaviour is documented protocol.

## Title

Provider events: `GetRequestReceived` doesn't show which child blobs a request will serve

## What happens

`provider::events` lets an application intercept requests (`RequestMode::Intercept`) and accept or reject them. A natural
access-control design is to check `request.hash` against a per-peer allow-list.

However, a `GetRequest` can carry ranges at offsets 1 and above. In that case `handle_get_impl` reads the requested blob as a
`HashSeq` and serves the blobs it names:

```rust
for (offset, ranges) in request.ranges.iter_non_empty_infinite() {
    if offset == 0 { send_blob(.., hash, ..) }
    else { let Some(hash) = hash_seq.get(offset - 1).await? else { break }; send_blob(.., hash, ..) }
}
```

Those child hashes never appear in any event. So an application that authorises `(peer, request.hash)` also, unknowingly,
authorises every blob named inside that blob. If peers can get blobs of their choosing onto the provider (any upload-style
application), a peer can store a "hash sequence" naming blobs it isn't allowed, request it with child ranges, and receive them.

## Our case

GlobalObject is a backup system where hosts store other users' encrypted pieces and serve them only against per-peer grants.
We found this while designing range grants. The data stays encrypted, but the attack bypasses accounting and bandwidth limits,
and reveals which blobs a host holds. We now accept only requests where `ranges.as_single() == Some((0, _))`.

## Suggested fixes (any one would do)

1. **Docs:** say on `GetRequestReceived` and `RequestMode::Intercept` that child blobs of hash-sequence requests are served
   without separate events, and show the `as_single()` check for single-blob-only providers.
2. **Option:** an `EventMask` setting (for example `hash_seq: Disabled`) that makes the provider refuse requests with child
   ranges.
3. **Event:** a per-child event (or a resolved child list in `GetRequestReceived`), so interceptors can authorise each blob
   that is actually sent.

Option 1 is a small PR we'd be happy to send. Options 2 and 3 need maintainer input on the event API.

## Repro outline

1. Provider with `get: RequestMode::Intercept`, allowing only hash `A`.
2. Store blob `A` = `HashSeq([B])`, and an unrelated blob `B`.
3. Client sends `GetRequest { hash: A, ranges: ChunkRangesSeq::from_ranges([empty, all]) }`.
4. The intercept sees only `A` and allows it, and the client receives `B`.

Our regression test is `v04_hash_sequence_requests_cannot_reach_other_blobs` in `crates/e2e/tests/grid.rs`.
