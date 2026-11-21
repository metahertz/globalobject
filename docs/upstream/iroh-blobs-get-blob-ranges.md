# Draft PR for `n0-computer/iroh-blobs`: `get_blob_ranges`

Status: **draft, not sent.** Written against 0.104.1. Check upstream `main` first, since the `get` module changes often and an
equivalent may already exist.

## Title

`get::request`: add `get_blob_ranges`, a streaming ranged counterpart to `get_blob`

## Description

`get::request::get_blob` streams a whole blob as verified `BaoContentItem`s. There's no equivalent for a *part* of a blob,
even though the protocol and provider support ranges (`GetRequest::blob_ranges`). Users who want a verified byte range without
importing it into a store have to drive `get::fsm` themselves:

```rust
let req = GetRequest::blob_ranges(hash, ranges);
let connected = fsm::start(conn, req, Default::default()).next().await?;
let fsm::ConnectedNext::StartRoot(root) = connected.next().await? else { unreachable!() };
let (mut content, size) = root.next().next().await?;
loop {
    match content.next().await {
        fsm::BlobContentNext::More((next, item)) => { /* leaves */ content = next; }
        fsm::BlobContentNext::Done(end) => { /* closing */ break; }
    }
}
```

This PR adds:

```rust
/// Like [`get_blob`], for the given chunk ranges of the blob only.
pub fn get_blob_ranges(connection: Connection, hash: Hash, ranges: ChunkRanges) -> GetBlobResult
```

It shares its body with `get_blob` (which becomes `get_blob_ranges(conn, hash, ChunkRanges::all())`), so the two can't drift
apart. `GetBlobResult::bytes()` and friends work unchanged and return the requested ranges' bytes.

## Tests

- **Requested ranges only:** a ranged request on a multi-chunk-group blob yields leaves covering exactly those ranges. That
  means whole chunk groups, which the docs state.
- **Bad data refused:** a corrupted provider produces `GetBlobItem::Error`, as for `get_blob`.
- **Equivalence:** `get_blob_ranges(.., ChunkRanges::all())` matches `get_blob`.

## Why we want it

GlobalObject serves partial reads of encrypted backup packs over `iroh-blobs`, with per-range grants enforced in a provider
event hook. We currently carry a copy of the `fsm` loop (`crates/net/src/lib.rs`, `fetch_range`). The helper would let us delete
it and stay on the supported surface.

## Size

About 30 lines plus tests. No breaking change.
