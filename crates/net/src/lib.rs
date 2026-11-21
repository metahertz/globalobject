//! Peer networking, on `iroh` (encrypted QUIC with hole punching and relays) and `iroh-blobs`
//! (BLAKE3 verified streaming). Everything received is checked against its hash on the way in.

use anyhow::{anyhow, bail, Context, Result};
use bao_tree::io::BaoContentItem;
use globalobject_proto::{PeerReply, PeerRequest};
use iroh::{endpoint::presets, Endpoint, EndpointAddr, RelayMode};
use iroh_blobs::{
    get::{
        fsm,
        request::{get_blob, GetBlobItem},
    },
    protocol::GetRequest,
    Hash,
};

pub use bao_tree::ChunkRanges;
use n0_future::StreamExt;
use std::time::Duration;

pub use iroh_blobs::ALPN as BLOBS_ALPN;
pub const PEER_ALPN: &[u8] = b"globalobject/peer/1";
const MAX_MESSAGE: usize = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NetMode {
    /// Loopback only, with no relays. For tests and local grids.
    Local,
    /// Real networks: hole punching, with relays as fallback.
    Internet,
}

pub async fn bind(mode: NetMode, alpns: Vec<Vec<u8>>) -> Result<Endpoint> {
    let ep = match mode {
        NetMode::Local => Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Disabled).bind_addr("127.0.0.1:0")?.alpns(alpns).bind().await?,
        NetMode::Internet => Endpoint::builder(presets::N0).alpns(alpns).bind().await?,
    };
    // Addresses appear shortly after binding, and callers immediately hand them to others.
    for _ in 0..100 {
        if !ep.addr().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(ep)
}

pub fn addr_string(ep: &Endpoint) -> String {
    serde_json::to_string(&ep.addr()).expect("serialize address")
}

pub fn parse_addr(s: &str) -> Result<EndpointAddr> {
    serde_json::from_str(s).context("bad peer address")
}

pub fn parse_hash(s: &str) -> Result<Hash> {
    s.parse().map_err(|_| anyhow!("bad hash"))
}

pub fn hash_of(data: &[u8]) -> String {
    Hash::new(data).to_hex().to_string()
}

/// One request and reply with a peer.
pub async fn peer_call(ep: &Endpoint, addr: &str, req: &PeerRequest) -> Result<PeerReply> {
    let conn = ep.connect(parse_addr(addr)?, PEER_ALPN).await.context("connecting to peer")?;
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(&serde_json::to_vec(req)?).await?;
    send.finish()?;
    let raw = recv.read_to_end(MAX_MESSAGE).await.context("reading peer reply")?;
    conn.close(0u32.into(), b"done");
    Ok(serde_json::from_slice(&raw)?)
}

/// Fetch a whole blob into memory, verified against its hash, refusing to take more than `max`.
pub async fn fetch_verified(ep: &Endpoint, addr: &str, hash: &str, max: u64) -> Result<Vec<u8>> {
    let hash = parse_hash(hash)?;
    let conn = ep.connect(parse_addr(addr)?, BLOBS_ALPN).await.context("connecting to peer")?;
    let mut stream = get_blob(conn.clone(), hash);
    let mut leaves: Vec<(u64, bytes::Bytes)> = vec![];
    let mut total = 0u64;
    loop {
        match stream.next().await {
            Some(GetBlobItem::Item(BaoContentItem::Leaf(leaf))) => {
                total += leaf.data.len() as u64;
                if total > max {
                    conn.close(1u32.into(), b"too large");
                    bail!("blob is larger than the {max} bytes allowed");
                }
                leaves.push((leaf.offset, leaf.data));
            }
            Some(GetBlobItem::Item(_)) => {}
            Some(GetBlobItem::Done(_)) => break,
            Some(GetBlobItem::Error(e)) => bail!("fetch failed: {e}"),
            None => bail!("fetch ended early"),
        }
    }
    conn.close(0u32.into(), b"done");
    leaves.sort_by_key(|l| l.0);
    let mut out = Vec::with_capacity(total as usize);
    for (_, data) in leaves {
        out.extend_from_slice(&data);
    }
    Ok(out)
}

/// `iroh-blobs` addresses blobs in 1 KiB chunks.
const CHUNK: u64 = 1024;
/// ...and sends whole 16 KiB chunk groups, which is what a range really costs to serve.
pub const GROUP: u64 = 16 * 1024;

/// The chunks covering bytes `[start, end)`.
pub fn chunk_ranges(start: u64, end: u64) -> ChunkRanges {
    ChunkRanges::from(bao_tree::ChunkNum(start / CHUNK)..bao_tree::ChunkNum(end.div_ceil(CHUNK)))
}

/// Bytes a host sends to serve `[start, end)` of a blob of `size` bytes: the range rounded out
/// to whole chunk groups. What a ranged read is charged, and the most it can be credited.
pub fn served_bytes(start: u64, end: u64, size: u64) -> u64 {
    let lo = start / GROUP * GROUP;
    let hi = (end.div_ceil(GROUP) * GROUP).min(size.max(end));
    hi.saturating_sub(lo)
}

/// The chunk ranges of a request for one blob alone. `None` for anything else, including
/// requests for the children of a hash sequence: those would make the provider serve blobs
/// named *inside* the requested one, which a grant for that blob must not unlock.
pub fn single_blob_ranges(req: &GetRequest) -> Option<ChunkRanges> {
    match req.ranges.as_single() {
        Some((0, r)) => Some(r.clone()),
        _ => None,
    }
}

/// Fetch bytes `[start, end)` of a blob, verified against its hash, and how many bytes were
/// transferred to get them (whole chunk groups, so a little more than asked for).
pub async fn fetch_range(ep: &Endpoint, addr: &str, hash: &str, start: u64, end: u64) -> Result<(Vec<u8>, u64)> {
    anyhow::ensure!(start < end, "empty range");
    let hash = parse_hash(hash)?;
    let conn = ep.connect(parse_addr(addr)?, BLOBS_ALPN).await.context("connecting to peer")?;
    let res = fetch_range_on(&conn, hash, start, end).await;
    conn.close(0u32.into(), b"done");
    res
}

async fn fetch_range_on(conn: &iroh::endpoint::Connection, hash: Hash, start: u64, end: u64) -> Result<(Vec<u8>, u64)> {
    let req = GetRequest::blob_ranges(hash, chunk_ranges(start, end));
    let connected = fsm::start(conn.clone(), req, Default::default()).next().await?;
    let fsm::ConnectedNext::StartRoot(root) = connected.next().await? else { bail!("unexpected reply to a single-blob request") };
    let (mut content, size) = root.next().next().await?;
    anyhow::ensure!(end <= size, "range ends at {end}, past the blob's {size} bytes");
    let mut out = vec![0u8; (end - start) as usize];
    let (mut filled, mut transferred) = (0u64, 0u64);
    loop {
        match content.next().await {
            fsm::BlobContentNext::More((next, item)) => {
                if let BaoContentItem::Leaf(leaf) = item? {
                    let len = leaf.data.len() as u64;
                    transferred += len;
                    anyhow::ensure!(transferred <= served_bytes(start, end, size) + GROUP, "host sent more than was asked for");
                    let (lo, hi) = (leaf.offset.max(start), (leaf.offset + len).min(end));
                    if lo < hi {
                        out[(lo - start) as usize..(hi - start) as usize].copy_from_slice(&leaf.data[(lo - leaf.offset) as usize..(hi - leaf.offset) as usize]);
                        filled += hi - lo;
                    }
                }
                content = next;
            }
            fsm::BlobContentNext::Done(end_blob) => {
                if let fsm::EndBlobNext::Closing(closing) = end_blob.next() {
                    closing.next().await?;
                }
                break;
            }
        }
    }
    anyhow::ensure!(filled == end - start, "only {filled} of {} bytes arrived", end - start);
    Ok((out, transferred))
}

/// Check one range of a blob against its hash, using only the hash. This is how audits work.
/// `Ok(true)` means verified bytes arrived, `Ok(false)` means the host failed to prove it,
/// and `Err` means the host couldn't be reached at all.
pub async fn audit_range(ep: &Endpoint, addr: &str, hash: &str, chunk: u64, chunks: u64) -> Result<bool> {
    let hash = parse_hash(hash)?;
    let conn = ep.connect(parse_addr(addr)?, BLOBS_ALPN).await.context("connecting to peer")?;
    let ranges = bao_tree::ChunkRanges::from(bao_tree::ChunkNum(chunk)..bao_tree::ChunkNum(chunk + chunks));
    let store = iroh_blobs::store::mem::MemStore::new();
    let req = iroh_blobs::protocol::GetRequest::blob_ranges(hash, ranges);
    let result = store.remote().execute_get(conn.clone(), req).await;
    conn.close(0u32.into(), b"done");
    if let Err(e) = &result {
        tracing::debug!("audit transfer failed: {e:#}");
    }
    Ok(matches!(result, Ok(stats) if stats.payload_bytes_read > 0))
}
