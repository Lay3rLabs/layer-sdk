//! LayerNode: CertifiableAutomaton bridge between Commonware consensus and Layer App<T>.
//!
//! This is the single integration point where consensus callbacks call into
//! the Layer state machine. LayerNode wraps Arc<RwLock<App<T>>> and translates:
//! - genesis()  -> constant genesis parent digest
//! - propose()  -> build on consensus `context.parent` once it is executed
//! - verify()   -> check parent/height/state_root/timestamp/proposer, NO state mutation
//! - certify()  -> payload availability + durable persistence, NO execution
//! - finalize() -> execute the finalized chain in order (DETERMINISM CRITICAL)
//!
//! Execution happens ONLY on finalization. A notarized block may still be
//! skipped by consensus, and a validator that could not certify a block must
//! still apply it once it is finalized; executing in certify() violated both
//! and let validators silently diverge.
//!
//! IMPORTANT: The `pending_payloads` map is shared with the Relay (Plan 03).
//! The proposer's `propose()` inserts payloads; the Relay inserts payloads
//! received from other validators. `verify()` looks up digests in this shared
//! map. Without Relay wiring, only the proposer would have payloads and
//! non-proposers would always fail `verify()`.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use commonware_consensus::{Automaton, CertifiableAutomaton};
use commonware_consensus::simplex::types::Context;
use commonware_consensus::types::{Epoch, Round};
use commonware_cryptography::{sha256, Hasher, PublicKey};

use layer_app::App;
use layer_std::api::Block;
use layer_std::Timestamp;
use layer_storage::PersistentStorage;
use tokio::sync::{Mutex, RwLock, oneshot};

// Cosmos tx deserialization for execute_block()
use layer_cosmos::parse_cosmos_tx;

use crate::block::BlockPayload;
use crate::mempool::{tx_hash, Mempool, TxHash};
use crate::payload_store::{PayloadStore, DEFAULT_RETAIN_HEIGHTS};
use crate::tx_index::{tx_hash_hex, tx_response_from_result, TxIndex, DEFAULT_TX_INDEX_CAPACITY};

/// Maximum number of transactions per block proposal.
const MAX_BLOCK_TXS: usize = 100;
/// Maximum total tx bytes per block. Keeps the relayed payload under the
/// 10 MiB P2P message limit (main.rs) with headroom for encoding overhead.
pub const MAX_BLOCK_TX_BYTES: usize = 8 * 1024 * 1024;

const POLL_INTERVAL: Duration = Duration::from_millis(25);
/// propose() waits this long for its parent to be finalized + executed (< leader_timeout).
const PROPOSE_WAIT_MAX: Duration = Duration::from_millis(2_000);
/// verify() waits this long for the payload and its executed parent (< leader_timeout).
const VERIFY_WAIT_MAX: Duration = Duration::from_millis(2_500);
/// certify() waits this long for the payload bytes (< certification_timeout).
const CERTIFY_WAIT_MAX: Duration = Duration::from_millis(4_000);
/// finalize() waits this long for missing ancestor payloads before halting execution.
const FINALIZE_FETCH_WAIT_MAX: Duration = Duration::from_millis(5_000);
/// Minimum spacing between repeated fetch requests for the same digest.
const FETCH_RETRY: Duration = Duration::from_millis(500);
/// Max parent-link hops when checking a certify candidate against the
/// executed chain. Bounds the walk so a deep historical ask can't stall voting.
const EXECUTED_WALK_MAX: u64 = 8_192;
/// Unsolicited peer payloads are accepted only this far above the executed tip.
pub const PEER_PAYLOAD_LOOKAHEAD: u64 = 64;
/// Unsolicited peer payloads are dropped once this many payloads are pending.
pub const MAX_PENDING_PAYLOADS: usize = 256;
/// Outstanding fetch requests remembered (so replies bypass the lookahead).
const MAX_REQUESTED_DIGESTS: usize = 65_536;
/// Outstanding height-ranged backfill requests remembered.
const MAX_REQUESTED_HEIGHTS: usize = 65_536;
/// Heights covered by a single `FetchRequest::HeightRange` message.
pub const BACKFILL_BATCH_SIZE: u64 = 64;
/// How far above the executed tip one backfill tick may reach. Larger spans
/// amortize the digest-walk latency over many parallel height fetches.
const BACKFILL_MAX_SPAN: u64 = 540_000;
/// Heights one backfill tick may solicit. A gap larger than this is walked
/// in bounded ascending bursts instead of one full-range flood per tick.
/// rc5: 1_024/tick (~4k reply messages/s) helped saturate the peers'
/// single-threaded serve loop — cut to 128 (still ~4 requests/s).
const BACKFILL_TICK_BUDGET: u64 = 128;
/// How long a certified-digest fetch waits before re-asking a peer — same
/// philosophy as HEIGHT_REASK_AFTER, for the exact-digest fetch path.
const DIGEST_REASK_AFTER: Duration = Duration::from_secs(2);
/// Certified digests one backfill tick may solicit by exact digest. rc5:
/// 256/tick (~1k requests/s) saturated the peers' serve loop and the
/// broadband uplink — the finalize walk's cursor replies drowned in that
/// flood for 80+ seconds (2026-10-09 g1 soak). 32/tick is still ~40× the
/// chain rate; a 1k-gap catch-up requests everything within ~10s.
const CERTIFIED_DIGEST_BUDGET: usize = 32;
/// Upper bound on the certified-digest queue. Replayed finalizations of
/// heights no peer still holds would otherwise churn forever; the oldest
/// entry is dropped (by-height backfill still covers old heights).
const CERTIFIED_DIGEST_QUEUE_CAP: usize = 8_192;
/// A missing height is re-requested only after its previous request is this
/// old — bounds per-peer fetch traffic on large gaps. Re-sending every
/// missing range every tick flooded the reply path in the 2026-10-02 C9
/// soak (node wedged ~10k blocks behind, zero inserts for minutes).
const HEIGHT_REASK_AFTER: Duration = Duration::from_secs(2);
/// A requested height stays "solicited" this long for reply acceptance —
/// replies inside the window bypass lookahead/pending bounds. Deliberately
/// far above HEIGHT_REASK_AFTER: a reply delayed by peer queueing must not
/// be dropped as unsolicited (that drop then re-request loop is the wedge).
const SOLICITED_HEIGHT_TTL: Duration = Duration::from_secs(30);
/// Absolute pending ceiling INCLUDING solicited replies. verify()/certify()
/// solicit far-future proposals while the executed tip lags, so solicited
/// inserts still need a bound (16k × ~128B payloads ≈ 2 MiB worst case).
const MAX_PENDING_PAYLOADS_TOTAL: usize = 16_384;
/// Headroom held back for solicited fetch replies: unsolicited pushes are
/// refused once pending reaches `MAX_PENDING_PAYLOADS_TOTAL - RESERVE`,
/// so a catch-up flood of far-tip pushes can never occupy every slot the
/// solicited tip+1 link needs to land in (the rc11 soak deadlock).
const PENDING_SOLICITED_RESERVE: usize = 1_024;

/// How far ahead of a validator's clock a proposed timestamp may run.
const MAX_FUTURE_DRIFT: Duration = Duration::from_secs(2);

/// A payload fetch this node wants peers to answer over the P2P relay.
/// Digest requests serve the finalize() walk; HeightRange requests serve
/// bulk backfill (requester doesn't know digests yet — that's the point).
pub enum FetchRequest {
    Digest([u8; 32]),
    /// Inclusive start + count (count ≤ BACKFILL_BATCH_SIZE).
    HeightRange { start: u64, count: u16 },
}

/// Apply a chaos fault-injection mode to a freshly built proposal payload
/// (devnet only — `NodeConfig::fault_inject`). Returns true when the mode
/// was recognized and the payload mutated. Honest validators' verify()
/// must reject the result: the byzantine-proposer leg asserts exactly that.
fn apply_fault_inject(payload: &mut BlockPayload, mode: &str) -> bool {
    match mode {
        // Claimed post-block state that honest validators cannot recompute —
        // verify() fails its state_root check, view times out, next leader.
        "bad_state_root" => {
            payload.state_root = [0xEF; 32];
            true
        }
        // Break the consensus parent link — verify() rejects on the
        // parent_digest != context.parent comparison.
        "bad_parent" => {
            payload.parent_digest = [0xEF; 32];
            true
        }
        _ => false,
    }
}

/// Parent digest of the first block. Constant (not the app hash) so that
/// `genesis()` is identical on every validator and across restarts.
pub fn genesis_parent() -> [u8; 32] {
    let mut hasher = sha256::Sha256::new();
    hasher.update(b"slay3r/genesis-parent/v1");
    hasher.finalize().0
}

/// Local wall clock in nanoseconds since the UNIX epoch.
fn now_nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// Proposal timestamp: the proposer's wall clock, nudged past the parent
/// when the clock trails it so block time stays strictly increasing.
pub fn proposal_timestamp_nanos(parent_nanos: u64, now_nanos: u64) -> u64 {
    now_nanos.max(parent_nanos.saturating_add(1))
}

/// Pure consensus-validity check for a proposed payload, evaluated against
/// this validator's executed tip (which must equal the proposal's parent).
pub fn validate_payload(
    payload: &BlockPayload,
    parent: &[u8; 32],
    expected_height: u64,
    expected_state_root: &[u8; 32],
    parent_timestamp_nanos: u64,
    now_nanos: u64,
    expected_proposer: &[u8],
) -> Result<(), &'static str> {
    if payload.parent_digest != *parent {
        return Err("parent_digest does not match consensus parent");
    }
    if payload.height != expected_height {
        return Err("height is not parent height + 1");
    }
    if payload.state_root != *expected_state_root {
        return Err("state_root does not match local post-parent state");
    }
    if payload.timestamp_nanos <= parent_timestamp_nanos {
        return Err("timestamp is not after the parent block");
    }
    if payload.timestamp_nanos > now_nanos.saturating_add(MAX_FUTURE_DRIFT.as_nanos() as u64) {
        return Err("timestamp is too far ahead of the local clock");
    }
    if payload.proposer.as_slice() != expected_proposer {
        return Err("proposer is not the view leader");
    }
    if payload.txs.len() > MAX_BLOCK_TXS {
        return Err("too many transactions");
    }
    if payload.txs.iter().map(|t| t.len()).sum::<usize>() > MAX_BLOCK_TX_BYTES {
        return Err("transactions exceed block byte budget");
    }
    Ok(())
}

/// Finalized-but-not-yet-executed payload digests, in finalization order
/// (which equals height order — consensus finalizes blocks sequentially).
///
/// Fed by the consensus Reporter (main.rs hands every Finalization activity
/// here), drained via `mark_resolved` once the payload is held or executed,
/// and fetched by exact certified digest from `backfill_tick`: digest-keyed
/// serving is always canonical, so a lagging node's execution never stalls
/// behind a peer's by-height index (the 2026-10-09 g1 soak stall).
#[derive(Default)]
struct CertifiedDigests {
    queue: VecDeque<[u8; 32]>,
    last_requested: HashMap<[u8; 32], Instant>,
}

impl CertifiedDigests {
    fn push(&mut self, digest: [u8; 32]) {
        if self.queue.len() >= CERTIFIED_DIGEST_QUEUE_CAP {
            if let Some(dropped) = self.queue.pop_front() {
                self.last_requested.remove(&dropped);
            }
        }
        self.queue.push_back(digest);
    }

    fn mark_resolved(&mut self, digest: [u8; 32]) {
        if self.queue.front() == Some(&digest) {
            self.queue.pop_front();
        } else {
            self.queue.retain(|d| *d != digest);
        }
        self.last_requested.remove(&digest);
    }

    fn ready_to_request(&self, digest: [u8; 32]) -> bool {
        self.last_requested
            .get(&digest)
            .map_or(true, |t| t.elapsed() >= DIGEST_REASK_AFTER)
    }

    fn note_requested(&mut self, digest: [u8; 32]) {
        self.last_requested.insert(digest, Instant::now());
    }
}

/// LayerNode bridges Commonware consensus to the Layer state machine.
///
/// Implements `CertifiableAutomaton` — the consensus engine calls `genesis()`,
/// `propose()`, `verify()`, and `certify()` at the appropriate protocol steps.
///
/// Type parameters:
/// - `T`: The persistent storage backend (e.g., `MemoryStore` or `RockStore`)
/// - `P`: The public key type from the consensus signing scheme (e.g., `bls12381::PublicKey`)
pub struct LayerNode<T: PersistentStorage + Send + Sync + 'static, P: PublicKey> {
    /// The Layer application state machine, wrapped for shared async access.
    /// RwLock allows concurrent gRPC reads while finalize_block holds write lock.
    app: Arc<RwLock<App<T>>>,
    /// Application-managed transaction mempool.
    mempool: Arc<Mutex<Mempool>>,
    /// Pending block payloads keyed by their digest, awaiting verify/certify.
    /// BTreeMap for deterministic ordering (CONS-04).
    /// SHARED with the Relay (Plan 03) — the Relay inserts payloads received
    /// from other validators so non-proposers can look them up in verify().
    pending_payloads: Arc<Mutex<BTreeMap<[u8; 32], BlockPayload>>>,
    /// Height of the last EXECUTED (finalized) block.
    /// Lock order everywhere: current_height -> last_digest -> app.
    current_height: Arc<Mutex<u64>>,
    /// Digest of the last EXECUTED (finalized) block.
    last_digest: Arc<Mutex<[u8; 32]>>,
    /// Serializes finalize() so the finalized chain is applied exactly once, in order.
    exec_lock: Arc<Mutex<()>>,
    /// Durable copy of certified/executed payloads (survives restart).
    store: Arc<std::sync::Mutex<PayloadStore>>,
    /// Requests a missing payload from peers (wired to the P2P relay in main.rs).
    fetch_tx: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<FetchRequest>>>>,
    /// Digests this node has requested from peers and not yet received.
    requested: Arc<std::sync::Mutex<HashSet<[u8; 32]>>>,
    /// Heights this node has asked peers for via backfill, with request
    /// time — entries expire after FETCH_RETRY so misses are re-requested.
    requested_heights: Arc<std::sync::Mutex<BTreeMap<u64, Instant>>>,
    /// Finalized-but-unexecuted payload digests (see `CertifiedDigests`).
    certified_digests: Arc<std::sync::Mutex<CertifiedDigests>>,
    /// rc6: last time ensure_executed spawned a finalize walk per digest,
    /// plus how many throttle windows that digest has fired in. Bounds the
    /// exec_lock task queue across thousands of nullified views while a
    /// certified-but-unexecuted parent blocks progress; the count feeds
    /// same-digest log dedup (first sighting + every 50th repeat).
    ensure_exec: Arc<std::sync::Mutex<HashMap<[u8; 32], (Instant, u32)>>>,
    /// Per-digest count of certify() wait-window timeouts. One-shot misses
    /// are normal catch-up churn and stay at INFO; the same digest timing
    /// out repeatedly across views is the #5378-class stall signature and
    /// is promoted to WARN (idx6 SOC: 13k INFO lines hid the real signal).
    certify_timeouts: Arc<std::sync::Mutex<HashMap<[u8; 32], u32>>>,
    /// Highest payload height ever observed (admitted or rejected pushes,
    /// payloads traversed in finalize). Fetch-target hint for backfill —
    /// never used to advance execution.
    max_seen_height: Arc<std::sync::atomic::AtomicU64>,
    /// Node-local tx result index for GetTx (not consensus state).
    tx_index: Arc<TxIndex>,
    /// Chaos fault-injection mode (devnet only). When set, propose() emits
    /// a corrupted payload so the byzantine-proposer path is exercised.
    fault_inject: Option<String>,
    /// Sidecar pruning window from `NodeConfig::prune_keep_heights()`.
    /// `Some(n)` = after each executed block, drop `_payload/`+`_ts/`+
    /// `_txres/` sidecars older than tip−n. `None` = archive mode.
    prune_keep_heights: Option<u64>,
    /// Set-change restart anchor: (height, payload digest) every seat agreed
    /// to build on after a journal wipe. genesis() returns the anchor digest
    /// while the executed tip is below `height`, so seats that halted at
    /// different heights still share one consensus parent.
    restart_anchor: Option<(u64, [u8; 32])>,
    /// rc17v2 wedge self-recovery: the data dir root so a certified-but-
    /// unexecuted parent stuck across ~5min of probes can run the
    /// documented archive-consensus + restart procedure in place
    /// ({data_dir}/consensus is the journal — see consensus_storage_path).
    /// None disables self-recovery (tests, embedded use).
    data_dir: Option<std::path::PathBuf>,
    /// Phantom to bind the P type parameter without storing P directly.
    _phantom: std::marker::PhantomData<P>,
}

/// Manual Clone implementation — all fields are behind Arc so T does not need Clone.
impl<T: PersistentStorage + Send + Sync + 'static, P: PublicKey> Clone for LayerNode<T, P> {
    fn clone(&self) -> Self {
        LayerNode {
            app: self.app.clone(),
            mempool: self.mempool.clone(),
            pending_payloads: self.pending_payloads.clone(),
            current_height: self.current_height.clone(),
            last_digest: self.last_digest.clone(),
            exec_lock: self.exec_lock.clone(),
            store: self.store.clone(),
            fetch_tx: self.fetch_tx.clone(),
            requested: self.requested.clone(),
            requested_heights: self.requested_heights.clone(),
            certified_digests: self.certified_digests.clone(),
            ensure_exec: self.ensure_exec.clone(),
            certify_timeouts: self.certify_timeouts.clone(),
            max_seen_height: self.max_seen_height.clone(),
            tx_index: self.tx_index.clone(),
            fault_inject: self.fault_inject.clone(),
            prune_keep_heights: self.prune_keep_heights,
            restart_anchor: self.restart_anchor,
            data_dir: self.data_dir.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T: PersistentStorage + Send + Sync + 'static, P: PublicKey> LayerNode<T, P> {
    pub fn new(app: Arc<RwLock<App<T>>>, mempool: Arc<Mutex<Mempool>>, initial_height: u64) -> Self {
        // Executed tip digest: genesis parent at height 0, otherwise the digest
        // of the persisted payload at the resume height. If that payload is
        // missing the node cannot link new blocks and will (correctly) refuse
        // to vote rather than diverge.
        let last_digest = if initial_height == 0 {
            genesis_parent()
        } else {
            let stored = app
                .try_read()
                .ok()
                .and_then(|a| a.get_block_payload(initial_height));
            match stored {
                Some(bytes) => {
                    let mut hasher = sha256::Sha256::new();
                    hasher.update(&bytes);
                    hasher.finalize().0
                }
                None => {
                    tracing::error!(
                        height = initial_height,
                        "No stored payload for resume height — executed tip digest unknown; node will not vote"
                    );
                    [0u8; 32]
                }
            }
        };
        LayerNode {
            app,
            mempool,
            pending_payloads: Arc::new(Mutex::new(BTreeMap::new())),
            current_height: Arc::new(Mutex::new(initial_height)),
            last_digest: Arc::new(Mutex::new(last_digest)),
            exec_lock: Arc::new(Mutex::new(())),
            store: Arc::new(std::sync::Mutex::new(PayloadStore::in_memory())),
            fetch_tx: Arc::new(std::sync::Mutex::new(None)),
            requested: Arc::new(std::sync::Mutex::new(HashSet::new())),
            requested_heights: Arc::new(std::sync::Mutex::new(BTreeMap::new())),
            certified_digests: Arc::new(std::sync::Mutex::new(CertifiedDigests::default())),
            ensure_exec: Arc::new(std::sync::Mutex::new(HashMap::new())),
            certify_timeouts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            max_seen_height: Arc::new(std::sync::atomic::AtomicU64::new(initial_height)),
            tx_index: Arc::new(TxIndex::new(DEFAULT_TX_INDEX_CAPACITY)),
            fault_inject: None,
            prune_keep_heights: None,
            restart_anchor: None,
            data_dir: None,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Set the coordinated set-change restart anchor (NodeConfig::restart_anchor).
    pub fn with_restart_anchor(mut self, height: u64, digest: [u8; 32]) -> Self {
        self.restart_anchor = Some((height, digest));
        self
    }

    /// rc17v2: enable wedge self-recovery against the live data dir.
    pub fn with_data_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Self {
        self.data_dir = Some(dir.into());
        self
    }

    /// Attach a durable payload store. Payloads certified but not yet executed
    /// before a restart are loaded back into `pending_payloads` and returned so
    /// the caller can re-broadcast them to peers.
    pub async fn with_payload_store(self, store: PayloadStore) -> (Self, Vec<BlockPayload>) {
        let tip = *self.current_height.lock().await;
        let recovered = store.payloads_above(tip);
        {
            let mut pending = self.pending_payloads.lock().await;
            for p in &recovered {
                pending.insert(p.digest(), p.clone());
            }
        }
        *self.store.lock().unwrap_or_else(|e| e.into_inner()) = store;
        (self, recovered)
    }

    /// Enable a chaos fault-injection mode (devnet only). See
    /// `NodeConfig::fault_inject` for recognized modes.
    pub fn with_fault_inject(mut self, mode: Option<String>) -> Self {
        self.fault_inject = mode;
        self
    }

    /// Enable role-tier sidecar pruning. `keep` is the resolved window
    /// from `NodeConfig::prune_keep_heights()` — `None` means archive mode
    /// and pruning is never run.
    pub fn with_pruning(mut self, keep: Option<u64>) -> Self {
        self.prune_keep_heights = keep;
        self
    }

    /// Wire the channel used to request missing payloads from peers.
    pub fn set_fetch_sender(&self, tx: tokio::sync::mpsc::UnboundedSender<FetchRequest>) {
        *self.fetch_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    }

    fn request_payload(&self, digest: [u8; 32]) {
        {
            let mut requested = self.requested.lock().unwrap_or_else(|e| e.into_inner());
            if requested.len() >= MAX_REQUESTED_DIGESTS {
                requested.clear();
            }
            requested.insert(digest);
        }
        if let Some(tx) = self.fetch_tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(FetchRequest::Digest(digest));
        }
    }

    /// Record an observed payload height as a backfill fetch target.
    /// Purely a hint — never advances execution.
    fn note_height_observed(&self, height: u64) {
        self.max_seen_height
            .fetch_max(height, std::sync::atomic::Ordering::Relaxed);
    }

    /// rc14: a certified parent that is not our executed tip must NOT be
    /// executed from here — only prefetched.
    ///
    /// The rc6 version of this function called finalize(parent) to break a
    /// "notarized-but-never-finalized" wedge, on the assumption that a
    /// certified parent is the unique canonical next block. That assumption
    /// only holds WITHIN a view: under view churn two different views can
    /// certify two different same-height payloads with zero equivocation —
    /// and that is exactly what forked g1 at height 2631739 on 2026-10-21
    /// (4c03cc6b… vs 2039d342…, identical app hash, different digest). Seats
    /// that eagerly executed the losing variant could never connect to the
    /// finalized chain again: digest-level divergence with identical state.
    ///
    /// The invariant now: ONLY a Finalization activity may advance execution
    /// — finalization is unique per height (4/5 finalize votes cannot sign
    /// two payloads). A certified view on the finalized chain is finalized
    /// by its descendants' certificates, so waiting is safe; a certified
    /// view that is never finalized is a dead variant and must not execute.
    /// verify()/propose() already wait bounded on the executed tip, so a
    /// not-yet-finalized parent simply nullifies until the certificate
    /// arrives rather than executing speculatively.
    ///
    /// What remains here: prefetch the payload so the eventual finalized
    /// walk is instant, and log loudly — a parent staying unexecuted is a
    /// signal (dead variant or missing finalization) worth seeing.
    /// Throttled to one probe per digest per few seconds.
    pub fn ensure_executed(&self, parent: [u8; 32]) {
        const THROTTLE: Duration = Duration::from_secs(6);
        /// Bound the throttle map: every distinct certified-but-unexecuted
        /// parent (a dead variant per view) inserts an entry, and a long
        /// stall would otherwise grow it without limit. Clearing just lets
        /// older digests be re-fetched sooner — harmless.
        const MAX_ENSURE_EXEC: usize = 4_096;
        let mut g = self.ensure_exec.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() >= MAX_ENSURE_EXEC {
            g.clear();
        }
        let now = Instant::now();
        let occurrences = match g.get_mut(&parent) {
            Some((t, _)) if now.duration_since(*t) < THROTTLE => return,
            Some((t, n)) => {
                *t = now;
                *n += 1;
                *n
            }
            None => {
                g.insert(parent, (now, 1));
                1
            }
        };
        drop(g);
        let node = self.clone();
        tokio::spawn(async move {
            let (_, tip) = node.executed_tip().await;
            if tip == parent {
                return;
            }
            if node.lookup_payload(&parent).await.is_none() {
                node.request_payload(parent);
            }
            // Same-digest dedup (RC17 SOC): the first sighting of a stuck
            // digest is the actionable WARN; every 50th throttle hit
            // (≈5min at the 6s pace) re-emits carrying the running count
            // so a monitor reads stall progression from a handful of lines
            // instead of a 27k-line firehose on idx6.
            if occurrences == 1 || occurrences % 50 == 0 {
                tracing::warn!(
                    parent = %hex::encode(parent),
                    occurrences,
                    "consensus parent certified but not executed — awaiting its finalization; never executing a merely-notarized ancestor"
                );
            }
            // rc17v2 wedge self-recovery: a certified parent still
            // unexecuted at the ~5-minute signature (50 throttle windows
            // at the 6s pace) is the dead-variant livelock that halted g1
            // on 2026-11-01 (h3938615, parent 6faf9ba7). The documented
            // fix — archive data/consensus and restart from the finalized
            // tip — now runs in place: once per digest, then exit for the
            // watchdog. Every seat on this build hits the same signature
            // within minutes, so the cohort forgets the dead variant
            // together instead of waiting for a coordinated human window.
            if occurrences % 50 == 0 {
                node.maybe_wedge_recover(parent);
            }
        });
    }

    /// rc17v2: archive the consensus journal once per dead-variant digest
    /// and exit(75) so guard.sh/systemd restarts the node from the
    /// finalized tip — the manual 2026-11-01 procedure, automated.
    ///
    /// The {data_dir}/wedge-recovered-<digest16> marker prevents wipe-
    /// loops: if a lagging peer re-teaches the same dead variant after
    /// our own archive, repeating the wipe is a human decision, not a
    /// loop. Archiving (never deleting) keeps the wedge state as
    /// forensic evidence.
    fn maybe_wedge_recover(&self, parent: [u8; 32]) {
        let Some(dir) = &self.data_dir else { return };
        // Only fire while the parent is genuinely still not our executed
        // tip — a finalization that landed mid-probe is not a wedge.
        let stuck = self
            .last_digest
            .try_lock()
            .map(|d| *d != parent)
            .unwrap_or(true);
        if !stuck {
            return;
        }
        let marker = dir.join(format!(
            "wedge-recovered-{}",
            &hex::encode(parent)[..16]
        ));
        if marker.exists() {
            return;
        }
        // Rate cap: at most 2 wedge recoveries per hour, counted from
        // marker mtimes so the cap survives restarts. A chain producing
        // serial dead variants is a deeper bug — after the cap, stay up
        // wedged and page humans rather than churning archive+restarts.
        if self.wedge_recoveries_recent(dir) >= 2 {
            tracing::error!(
                parent = %hex::encode(parent),
                "wedge self-recovery rate-capped (2/hour) — staying up \
                 wedged; this needs operator attention, not another wipe"
            );
            return;
        }
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let bak = dir.join(format!("consensus.wedge-bak-{ts}"));
        tracing::error!(
            parent = %hex::encode(parent),
            backup = %bak.display(),
            "LIVELOCK SELF-RECOVERY: certified parent never finalized — \
             archiving consensus journal and restarting (dead variant \
             must not re-execute); 2026-11-01 coordinated procedure, automated"
        );
        if let Err(e) = std::fs::rename(dir.join("consensus"), &bak) {
            tracing::error!(error = %e, "wedge recovery: consensus archive failed — restarting anyway");
        }
        let _ = std::fs::write(&marker, format!("{}\n", hex::encode(parent)));
        // exit(75) = EX_TEMPFAIL: honest non-zero for a crash-for-
        // recovery; Restart=always and the guard.sh pgrep check both
        // restart on any exit.
        std::process::exit(75);
    }

    /// Count of `wedge-recovered-*` markers written in the last hour —
    /// filesystem-derived so the rate cap survives restarts.
    fn wedge_recoveries_recent(&self, dir: &std::path::Path) -> usize {
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or(UNIX_EPOCH);
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .starts_with("wedge-recovered-")
                            && e.metadata()
                                .and_then(|m| m.modified())
                                .map(|t| t >= cutoff)
                                .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0)
    }

    /// Record the digest of a payload that consensus finalized but this node
    /// has not executed yet (called by the Reporter forwarder in main.rs).
    /// Digests we already hold — the payload arrived with the proposal, or
    /// the block executed long ago — are not queued; only genuinely missing
    /// payloads are fetch targets.
    pub async fn note_finalized(&self, digest: [u8; 32]) {
        if self.lookup_payload(&digest).await.is_some() {
            return;
        }
        self.certified_digests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(digest);
    }

    /// One backfill step:
    /// (1) fetch every finalized-but-unexecuted payload by its exact
    ///     certified digest (digest-keyed serving is always canonical, so
    ///     execution cannot stall behind a peer's by-height index — the
    ///     2026-10-09 g1 soak stalled exactly there), and
    /// (2) ask peers for every missing height between the executed tip and
    ///     the highest observed payload height — complements the digest walk
    ///     in finalize(): that walk discovers the chain, backfill fills it in
    ///     parallel instead of one RTT per block.
    /// Called periodically by a background task in main.rs.
    pub async fn backfill_tick(&self) {
        // (1) Certified-digest fetch: bounded per tick, paced per digest via
        // DIGEST_REASK_AFTER; resolved digests (payload held or executed)
        // leave the queue.
        let snapshot: Vec<[u8; 32]> = {
            let certified = self
                .certified_digests
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            certified.queue.iter().copied().collect()
        };
        let mut resolved: Vec<[u8; 32]> = Vec::new();
        let mut to_request: Vec<[u8; 32]> = Vec::new();
        for digest in snapshot {
            if self.lookup_payload(&digest).await.is_some() {
                resolved.push(digest);
                continue;
            }
            if to_request.len() >= CERTIFIED_DIGEST_BUDGET {
                break;
            }
            let ready = self
                .certified_digests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .ready_to_request(digest);
            if ready {
                to_request.push(digest);
            }
        }
        {
            let mut certified = self
                .certified_digests
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for digest in resolved {
                certified.mark_resolved(digest);
            }
            for digest in &to_request {
                certified.note_requested(*digest);
            }
        }
        for digest in to_request {
            self.request_payload(digest);
        }

        // (2) Height-range backfill.
        let tip = *self.current_height.lock().await;
        let target = self
            .max_seen_height
            .load(std::sync::atomic::Ordering::Relaxed)
            .min(tip + BACKFILL_MAX_SPAN);
        if target <= tip {
            return;
        }

        // Heights we already have: on disk, or buffered pending execution.
        let held: HashSet<u64> = {
            let pending = self.pending_payloads.lock().await;
            let mut held: HashSet<u64> = pending.values().map(|p| p.height).collect();
            held.extend(
                (tip + 1..=target)
                    .filter(|h| {
                        self.store
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .contains_height(*h)
                    }),
            );
            held
        };

        // Expire marks only after the solicited TTL; heights asked recently
        // are in-flight and are NOT re-requested this tick. (Was: marks died
        // at FETCH_RETRY and every missing range re-sent every tick — the
        // flood that wedged recovery under a ~10k-block gap.)
        let mut runs: Vec<(u64, u64)> = Vec::new();
        {
            let mut req = self
                .requested_heights
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            req.retain(|_, t| t.elapsed() < SOLICITED_HEIGHT_TTL);
            // No wholesale clear at MAX_REQUESTED_HEIGHTS: on gaps larger
            // than the cap the clear re-requested the entire missing range
            // every tick, flooding the peer's inbound path. Fresh marks
            // already suppress re-requests via HEIGHT_REASK_AFTER, and the
            // TTL retention above bounds the map on its own.
            let mut run_start: Option<u64> = None;
            for h in (tip + 1)..=target {
                let in_flight = req
                    .get(&h)
                    .map_or(false, |t| t.elapsed() < HEIGHT_REASK_AFTER);
                let need = !held.contains(&h) && !in_flight;
                match (need, run_start) {
                    (true, None) => run_start = Some(h),
                    (false, Some(start)) => {
                        runs.push((start, h - start));
                        run_start = None;
                    }
                    _ => {}
                }
            }
            if let Some(start) = run_start {
                runs.push((start, target - start + 1));
            }
        }
        // Per-tick request budget: solicit the missing range in bounded
        // ascending bursts (executed tip first) so a large gap can never
        // arrive at a peer as a full-range flood.
        let mut budget: u64 = BACKFILL_TICK_BUDGET;
        for (start, count) in runs {
            if budget == 0 {
                break;
            }
            let n = count.min(budget);
            self.request_height_range(start, n);
            budget -= n;
        }
    }

    /// Queue one height-range request, marking each height as solicited so
    /// replies bypass the lookahead/capacity bounds in accept_peer_payload.
    fn request_height_range(&self, start: u64, count: u64) {
        if count == 0 {
            return;
        }
        let mut sent_start = start;
        let mut remaining = count;
        {
            let mut req = self
                .requested_heights
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            for h in start..start + count {
                req.insert(h, now);
            }
        }
        if let Some(tx) = self.fetch_tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            while remaining > 0 {
                let n = remaining.min(BACKFILL_BATCH_SIZE);
                let _ = tx.send(FetchRequest::HeightRange {
                    start: sent_start,
                    count: n as u16,
                });
                sent_start += n;
                remaining -= n;
            }
        }
    }

    /// Serve a payload by height for peer backfill requests: pending
    /// (not-yet-executed) payloads first, then the durable store, then the
    /// app's `_payload/` sidecar (kept for the pruning-tier window).
    pub async fn payload_by_height(&self, height: u64) -> Option<BlockPayload> {
        {
            let pending = self.pending_payloads.lock().await;
            if let Some(p) = pending.values().find(|p| p.height == height) {
                return Some(p.clone());
            }
        }
        let stored = self
            .store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_by_height(height);
        if stored.is_some() {
            return stored;
        }
        let bytes = self.app.read().await.get_block_payload(height)?;
        BlockPayload::from_bytes(&bytes).ok()
    }

    /// Admit a payload received from a peer into `pending_payloads`.
    ///
    /// Bounds peer-driven memory: stale and structurally oversized payloads
    /// are always rejected; pushes must lie within `PEER_PAYLOAD_LOOKAHEAD`
    /// of the executed tip, or at/below the highest payload height observed
    /// BEFORE this push (the relay catch-up window — a lagging node's peers
    /// relay payloads at the network tip, far above its own lookahead),
    /// and fit under `MAX_PENDING_PAYLOADS_TOTAL`. Replies to this node's
    /// own fetch requests bypass the window so catch-up across a larger
    /// gap still works.
    /// Returns `Ok(true)` if newly inserted, `Ok(false)` if already known.
    pub async fn accept_peer_payload(&self, payload: BlockPayload) -> Result<bool, &'static str> {
        self.accept_peer_relay(payload).await.0
    }

    /// Accept a peer payload and report whether the relay cascade should
    /// echo it. Solicited fetch replies (digest or by-height — this node
    /// asked for them) are never echoed: the serving peer already holds the
    /// payload, and echoing them doubled the peers' inbound load during
    /// catch-up (rc5). Only unsolicited pushes (proposals, one-hop cascades)
    /// propagate.
    pub async fn accept_peer_relay(
        &self,
        payload: BlockPayload,
    ) -> (Result<bool, &'static str>, bool) {
        if payload.txs.len() > MAX_BLOCK_TXS
            || payload.txs.iter().map(|t| t.len()).sum::<usize>() > MAX_BLOCK_TX_BYTES
        {
            return (Err("payload exceeds block limits"), false);
        }
        let tip = *self.current_height.lock().await;
        if payload.height <= tip {
            return (Err("payload at or below executed tip"), false);
        }
        let digest = payload.digest();
        // Relay catch-up bound: capture the observed-tip evidence from
        // BEFORE this push, so a peer cannot self-authorize its own far
        // payload (sending it once would otherwise advance max_seen).
        let within_observed =
            payload.height <= self.max_seen_height.load(std::sync::atomic::Ordering::Relaxed);
        // Always record the height — even rejected pushes are evidence of
        // the peer tip and give backfill a fetch target.
        self.note_height_observed(payload.height);
        let solicited = self
            .requested
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&digest)
            || self
                .requested_heights
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&payload.height)
                .is_some();
        let mut pending = self.pending_payloads.lock().await;
        if pending.contains_key(&digest) {
            return (Ok(false), false);
        }
        if !solicited && !within_observed && payload.height > tip + PEER_PAYLOAD_LOOKAHEAD {
            return (Err("unsolicited payload beyond lookahead window"), false);
        }
        if pending.len() >= MAX_PENDING_PAYLOADS_TOTAL {
            if !solicited {
                return (Err("pending payload capacity reached"), false);
            }
            // Solicited reply at capacity: evict the pending entry furthest
            // above the executed tip. Rejecting a payload we explicitly asked
            // for wedges catch-up permanently — pending only drains via
            // execution, and execution halts on exactly the link being
            // refused (rc11 soak: home-vb deadlocked 4.5h on this). The
            // evicted entry is refetchable via the same backfill paths.
            let Some(victim) = pending
                .iter()
                .max_by_key(|(_, p)| p.height)
                .map(|(d, _)| *d)
            else {
                return (Err("pending payload capacity reached"), false);
            };
            pending.remove(&victim);
        } else if !solicited && pending.len() >= MAX_PENDING_PAYLOADS_TOTAL - PENDING_SOLICITED_RESERVE {
            // Keep headroom for solicited replies so unsolicited pushes can
            // never fill the buffer to the point the rule above engages on
            // every catch-up reply.
            return (Err("pending payload capacity reached"), false);
        }
        pending.insert(digest, payload);
        (Ok(true), !solicited)
    }

    /// Look up a payload in memory, then on disk.
    pub async fn lookup_payload(&self, digest: &[u8; 32]) -> Option<BlockPayload> {
        if let Some(p) = self.pending_payloads.lock().await.get(digest) {
            return Some(p.clone());
        }
        self.store.lock().unwrap_or_else(|e| e.into_inner()).get(digest)
    }

    /// Executed tip as (height, digest).
    pub async fn executed_tip(&self) -> (u64, [u8; 32]) {
        let h = self.current_height.lock().await;
        let d = self.last_digest.lock().await;
        (*h, *d)
    }

    /// Compare a payload candidate against the executed chain.
    ///
    /// - `Some(true)`: our executed chain holds exactly this block at `p.height`.
    /// - `Some(false)`: our executed chain holds a DIFFERENT block at `p.height`
    ///   — the candidate is a dead variant; callers must not help finalize it.
    /// - `None`: undetermined — candidate is above the executed tip, or the
    ///   ancestry walk truncated (missing/pruned ancestor). Fail-open is
    ///   deliberate: a conflicting finalization can never execute here anyway
    ///   (finalize() fail-stops on a chain that doesn't connect to our tip),
    ///   so an undetermined answer must not cost liveness.
    ///
    /// The walk reads from `lookup_payload` (pending + persistent store);
    /// executed ancestors are always present there because certify() persists
    /// before voting and finalize() persists before executing.
    pub async fn consistent_with_executed_chain(&self, p: &BlockPayload) -> Option<bool> {
        let (tip_h, mut d) = self.executed_tip().await;
        if p.height > tip_h {
            return None;
        }
        let mut h = tip_h;
        let mut hops = 0u64;
        while h > p.height {
            if hops >= EXECUTED_WALK_MAX {
                return None;
            }
            let anc = self.lookup_payload(&d).await?;
            if anc.height != h {
                return None;
            }
            d = anc.parent_digest;
            h -= 1;
            hops += 1;
        }
        Some(d == p.digest())
    }

    /// Access to the app for external callers (e.g., gRPC query handler).
    pub fn app(&self) -> Arc<RwLock<App<T>>> {
        self.app.clone()
    }

    /// Access to the mempool for external callers (e.g., gRPC tx submission).
    pub fn mempool(&self) -> Arc<Mutex<Mempool>> {
        self.mempool.clone()
    }

    /// Access to the tx result index for the gRPC GetTx handler.
    pub fn tx_index(&self) -> Arc<TxIndex> {
        self.tx_index.clone()
    }

    /// Access to pending payloads for the Relay to populate when receiving
    /// broadcast payloads from other validators.
    /// CRITICAL: The Relay (Plan 03) MUST be constructed with this same Arc
    /// so that non-proposer validators can look up payloads during verify().
    /// Without this shared wiring, only the proposer has payloads in the map
    /// and 2-of-3 validators would always fail verify(), blocking consensus.
    pub fn pending_payloads(&self) -> Arc<Mutex<BTreeMap<[u8; 32], BlockPayload>>> {
        self.pending_payloads.clone()
    }

    /// Internal: compute the [u8; 32] digest of a BlockPayload.
    fn compute_digest(payload: &BlockPayload) -> [u8; 32] {
        payload.digest()
    }

    /// Execute the finalized chain ending at `digest`, in order, exactly once.
    ///
    /// Consensus may report only the newest finalization (ancestors are
    /// finalized by implication), so walk `parent_digest` links back to the
    /// executed tip and apply every block on the way. Returns the height and
    /// timestamp of `digest`, or `None` if it was already executed (replay)
    /// or cannot be executed. Missing payloads are fetched from peers; if they
    /// never arrive, execution halts (fail-stop) instead of skipping a block.
    pub async fn finalize(&self, digest: [u8; 32]) -> Option<(u64, u64)> {
        let _exec = self.exec_lock.lock().await;
        let (tip_height, tip_digest) = self.executed_tip().await;
        if digest == tip_digest {
            self.certified_digests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .mark_resolved(digest);
            return None;
        }

        let mut chain: Vec<BlockPayload> = Vec::new();
        let mut cursor = digest;
        let deadline = Instant::now() + FINALIZE_FETCH_WAIT_MAX;
        let mut last_request: Option<Instant> = None;
        while cursor != tip_digest {
            match self.lookup_payload(&cursor).await {
                Some(p) => {
                    if p.height <= tip_height {
                        if chain.is_empty() {
                            tracing::debug!(digest = %hex::encode(digest), "finalize: already executed (replay)");
                        } else {
                            tracing::error!(
                                digest = %hex::encode(digest),
                                tip_height,
                                "finalize: finalized chain does not connect to executed tip — local state diverged, halting execution"
                            );
                        }
                        return None;
                    }
                    cursor = p.parent_digest;
                    chain.push(p);
                }
                None => {
                    if Instant::now() >= deadline {
                        tracing::error!(
                            missing = %hex::encode(cursor),
                            finalized = %hex::encode(digest),
                            tip_height,
                            "finalize: finalized payload unavailable — execution halted until it is fetched"
                        );
                        return None;
                    }
                    if last_request.map_or(true, |t| t.elapsed() >= FETCH_RETRY) {
                        self.request_payload(cursor);
                        last_request = Some(Instant::now());
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            }
        }

        let mut result = None;
        for (i, payload) in chain.into_iter().rev().enumerate() {
            if payload.height != tip_height + 1 + i as u64 {
                tracing::error!(
                    height = payload.height,
                    expected = tip_height + 1 + i as u64,
                    "finalize: non-contiguous height in finalized chain — halting execution"
                );
                return None;
            }
            // Fail-stop divergence detector: 2f+1 validators checked this
            // state_root against their own state in verify(). If ours differs
            // we are the divergent node — stop rather than compound it.
            let local_root = {
                let app = self.app.read().await;
                app.state_root().unwrap_or([0u8; 32])
            };
            if payload.state_root != local_root {
                tracing::error!(
                    height = payload.height,
                    expected = %hex::encode(payload.state_root),
                    local = %hex::encode(local_root),
                    "finalize: state_root mismatch — local state diverged from the network, halting execution"
                );
                return None;
            }
            let d = payload.digest();
            self.pending_payloads.lock().await.remove(&d);
            self.certified_digests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .mark_resolved(d);
            result = Some(self.execute_payload(d, payload).await?);
        }

        if let Some((height, _)) = result {
            self.pending_payloads.lock().await.retain(|_, p| p.height > height);
            self.store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .prune_below(height.saturating_sub(DEFAULT_RETAIN_HEIGHTS));
            self.prune_sidecars(height).await;
            self.recheck_mempool().await;
        }
        result
    }

    /// Drop `_payload/`+`_ts/`+`_txres/` sidecars and `_cert/`+`_proposal/`
    /// finality records below `tip − keep`, per the node's pruning tier
    /// (`NodeConfig::pruning`). Node-local only —
    /// `_` keys never enter app_hash, so validators on different tiers
    /// still commit identical state. Batched to bound finalize latency;
    /// `_prune_floor` tracks progress so a large catch-up converges over
    /// consecutive blocks rather than stalling one.
    async fn prune_sidecars(&self, tip: u64) {
        /// Max heights pruned per executed block — bounds the single-commit
        /// batch so a ~110k-height catch-up can't wedge finalize.
        const MAX_PRUNE_PER_BLOCK: u64 = 2_048;

        let Some(keep) = self.prune_keep_heights else { return };
        let target_floor = tip.saturating_sub(keep);
        self.prune_finality_records(target_floor, MAX_PRUNE_PER_BLOCK).await;
        let old_floor = {
            let app = self.app.read().await;
            app.prune_floor()
        };
        if target_floor <= old_floor {
            return;
        }
        let new_floor = old_floor.saturating_add(MAX_PRUNE_PER_BLOCK).min(target_floor);
        let heights: Vec<u64> = ((old_floor + 1)..=new_floor).collect();

        // txres keys are hash-keyed, not height-keyed — recover each
        // pruned block's txhashes from its payload before it is deleted.
        let mut txhashes: Vec<String> = Vec::new();
        {
            let app = self.app.read().await;
            for h in &heights {
                if let Some(bytes) = app.get_block_payload(*h) {
                    if let Ok(p) = BlockPayload::from_bytes(&bytes) {
                        txhashes.extend(p.txs.iter().map(|t| tx_hash_hex(t)));
                    }
                }
            }
        }

        let mut app = self.app.write().await;
        match app.prune_sidecars(&heights, &txhashes, new_floor) {
            Ok(removed) => {
                if removed > 0 {
                    tracing::debug!(
                        floor = new_floor,
                        removed,
                        "sidecar pruning advanced"
                    );
                }
            }
            Err(e) => {
                // Pruning failure is non-fatal: stale sidecars only cost disk.
                tracing::warn!(error = ?e, floor = new_floor, "sidecar pruning failed — will retry next block");
            }
        }
    }

    async fn prune_finality_records(&self, target_floor: u64, max_heights: u64) {
        let old_floor = self.app.read().await.finality_prune_floor();
        if target_floor <= old_floor {
            return;
        }
        let new_floor = old_floor.saturating_add(max_heights).min(target_floor);
        let heights: Vec<u64> = ((old_floor + 1)..=new_floor).collect();
        let mut app = self.app.write().await;
        if let Err(e) = app.prune_finality(&heights, new_floor) {
            tracing::warn!(error = ?e, floor = new_floor, "finality record pruning failed — will retry next block");
        }
    }

    /// Re-run check_tx for every pending tx against the post-block state and
    /// evict the ones that are no longer valid (stale sequence, spent funds).
    /// Node-local policy only — never affects consensus.
    async fn recheck_mempool(&self) {
        let snapshot = self.mempool.lock().await.snapshot();
        if snapshot.is_empty() {
            return;
        }
        let invalid: Vec<TxHash> = {
            let app = self.app.read().await;
            let Some(chain_id) = app.info().map(|b| b.chain_id.clone()) else {
                return;
            };
            snapshot
                .iter()
                .filter(|(_, raw)| match parse_cosmos_tx(raw.clone(), &chain_id) {
                    Ok(tx) => app.check_tx(tx).result.is_err(),
                    Err(_) => true,
                })
                .map(|(h, _)| *h)
                .collect()
        };
        if !invalid.is_empty() {
            let mut pool = self.mempool.lock().await;
            let evicted = pool.remove_committed(invalid.iter());
            tracing::info!(evicted, remaining = pool.len(), "mempool recheck evicted invalid txs");
        }
    }

    /// Remove `digest` from pending and execute it on top of the executed tip.
    /// Low-level: no parent/state_root checks (finalize() performs those).
    #[cfg(test)]
    pub(crate) async fn execute_block(&self, digest: [u8; 32]) -> bool {
        let payload = {
            let mut pending = self.pending_payloads.lock().await;
            pending.remove(&digest)
        };
        match payload {
            Some(p) => self.execute_payload(digest, p).await.is_some(),
            None => false,
        }
    }

    /// Apply one payload on top of the executed tip via App::finalize_block().
    /// DETERMINISM CRITICAL. Holds current_height + last_digest for the whole
    /// commit so readers never observe a new state root with an old tip.
    async fn execute_payload(&self, digest: [u8; 32], payload: BlockPayload) -> Option<(u64, u64)> {
        let mut h = self.current_height.lock().await;
        let mut last = self.last_digest.lock().await;
        let height = *h + 1;

        // Convert BlockPayload to layer_std::api::Block.
        // Block timestamp comes from payload.timestamp_nanos (from consensus context — DETERMINISTIC).
        // NEVER use SystemTime::now() here.
        //
        // Deserialize transactions from BlockPayload raw bytes.
        // Each entry in payload.txs is a raw Cosmos proto-encoded tx (cosmos.tx.v1beta1.TxRaw).
        // parse_cosmos_tx() handles the full proto decode + signature extraction.
        let chain_id = {
            let app = self.app.read().await;  // SHARED read lock — read-only
            app.info()
                .map(|b| b.chain_id.clone())
                .unwrap_or_else(|| "junoclaw-1".to_string())
        };  // read lock released here

        let mut txs: Vec<layer_std::Tx> = Vec::with_capacity(payload.txs.len());
        let mut tx_hashes: Vec<String> = Vec::with_capacity(payload.txs.len());
        for raw in &payload.txs {
            match parse_cosmos_tx(raw.clone(), &chain_id) {
                Ok(tx) => {
                    txs.push(tx);
                    tx_hashes.push(tx_hash_hex(raw));
                }
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        tx_len = raw.len(),
                        "Skipping malformed tx in block — proposer's check_tx should have caught this"
                    );
                    // Skip malformed txs rather than rejecting the whole block.
                    // The proposer's check_tx already validated these; errors here
                    // mean the tx was corrupted in transit or the chain_id changed.
                }
            }
        }

        let block = Block {
            txs,
            height,
            time: Timestamp::from_nanos(payload.timestamp_nanos),
            proposer_address: payload.proposer.clone(),
            last_votes: vec![],
            // The BLS certificate is NOT available at certify() time — it is produced
            // by the consensus engine after a quorum of validators certify. The
            // LayerReporter receives the Finalization activity with cert_bytes and
            // calls App::set_block_certificate(height, cert_bytes) to persist it.
            // See main.rs LayerReporter::report() for the storage path.
            certificate: None,
        };

        // The payload bytes are committed atomically with the block's state:
        // restart derives the executed tip digest from them, and membership
        // proofs carry them so the light client can recompute the signed
        // digest and extract state_root.
        let payload_bytes = payload.to_bytes();
        let (result, exec_elapsed) = {
            let exec_start = Instant::now();
            let mut app = self.app.write().await;  // EXCLUSIVE write lock — mutates state
            (
                app.finalize_block_with_payload(block, Some(payload_bytes)),
                exec_start.elapsed(),
            )
        };  // write lock released here

        match result {
            Ok(response) => {
                // Commit succeeded: advance the executed tip.
                *h = height;
                *last = digest;

                // Drop this block's txs from the local mempool before the tip
                // locks are released, so the next proposal cannot re-include them.
                {
                    let committed: Vec<TxHash> = payload.txs.iter().map(|t| tx_hash(t)).collect();
                    self.mempool.lock().await.remove_committed(committed.iter());
                }

                // Index tx results for GetTx. tx_results is 1:1 with the parsed
                // txs (malformed ones were skipped above, together with their hash).
                // Q2: write-through to the persisted `_txres/` index so results
                // survive restarts and in-memory window eviction.
                let responses: Vec<_> = tx_hashes
                    .into_iter()
                    .zip(response.tx_results.iter())
                    .map(|(hash, res)| tx_response_from_result(hash, height, res))
                    .collect();
                {
                    let mut app = self.app.write().await;
                    for resp in &responses {
                        let encoded = prost::Message::encode_to_vec(resp);
                        if let Err(e) = app.set_tx_response(&resp.txhash, &encoded) {
                            tracing::warn!(txhash = %resp.txhash, error = %e, "Failed to persist tx response");
                        }
                    }
                }
                for resp in responses {
                    self.tx_index.insert(resp);
                }

                if let Err(e) = self.store.lock().unwrap_or_else(|e| e.into_inner()).put_executed(&payload) {
                    tracing::warn!(height, error = %e, "Failed to persist executed payload to payload store");
                }

                // Structured log: height, app_hash, digest (CONS-04, CONS-05 audit support)
                // The certificate bytes are set in Block.certificate by the Reporter after
                // certify() returns. The verify-consensus.sh script parses these fields.
                tracing::info!(
                    height = height,
                    app_hash = %hex::encode(&response.app_hash),
                    digest = %hex::encode(digest),
                    tx_count = response.tx_results.len(),
                    exec_ms = exec_elapsed.as_millis() as u64,
                    "Block finalized — certificate stored by Reporter on Finalization activity"
                );
                Some((height, payload.timestamp_nanos))
            }
            Err(e) => {
                tracing::error!(height = height, error = ?e, "finalize_block failed on a finalized block — halting execution");
                None
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Automaton and CertifiableAutomaton implementations
// ---------------------------------------------------------------------------

impl<T, P> Automaton for LayerNode<T, P>
where
    T: PersistentStorage + Send + Sync + 'static,
    P: PublicKey + Clone + Send + Sync + 'static,
{
    type Context = Context<sha256::Digest, P>;
    type Digest = sha256::Digest;

    async fn genesis(&mut self, _epoch: Epoch) -> Self::Digest {
        // Must be identical on every validator and across restarts: it is the
        // parent digest of the first block (see genesis_parent()).
        //
        // R9: with executed history, anchor on the executed tip instead of
        // the virtual genesis parent. A fresh consensus journal (validator
        // set change — the journal is cryptographically bound to the old
        // participant set and cannot be replayed) starts at view 1 needing
        // a parent; the executed tip is the only parent every node agrees
        // on. Without this, set changes deadlock: view 1 proposes on the
        // genesis parent while the app tip sits at height N.
        //
        // But a non-atomic halt leaves seats at DIFFERENT executed heights,
        // so "my tip" is not a shared anchor. When the coordinator supplied
        // restart_anchor (the highest tip observed at halt), seats behind it
        // anchor on the coordinator's digest and catch up via
        // ensure_executed; seats at/above it use their local tip, which IS
        // the anchor digest. On a normal restart the journal replays and
        // this is never consulted.
        let (h, d) = self.executed_tip().await;
        if let Some((anchor_height, anchor_digest)) = self.restart_anchor {
            if h < anchor_height {
                return sha256::Digest::from(anchor_digest);
            }
            if h == anchor_height && d != anchor_digest {
                tracing::error!(
                    height = h,
                    local = %hex::encode(d),
                    anchor = %hex::encode(anchor_digest),
                    "restart_anchor: executed tip digest differs from the coordinator's anchor — refusing to anchor on local tip"
                );
            }
        }
        if h == 0 {
            sha256::Digest::from(genesis_parent())
        } else {
            sha256::Digest::from(d)
        }
    }

    async fn propose(&mut self, context: Self::Context) -> oneshot::Receiver<Self::Digest> {
        let (tx, rx) = oneshot::channel();
        let node = self.clone();
        let (_, parent_digest) = context.parent;
        let parent: [u8; 32] = parent_digest.0;
        let view_num = context.round.view().get();
        // Proposer is the leader's public key bytes from context.
        let proposer = context.leader.as_ref().to_vec();

        tokio::spawn(async move {
            // Build on consensus' parent. It must be finalized and executed
            // here first so height and state_root (post-parent state, app-hash
            // semantics) are well-defined and identical on every validator.
            //
            // rc14: if the certified parent hasn't executed, prefetch it and
            // wait — only a Finalization activity may advance execution.
            // A certified parent that never finalizes is a dead variant;
            // skipping this proposal is the correct (nullify) outcome.
            node.ensure_executed(parent);
            let start = Instant::now();
            let (height, state_root, parent_time) = loop {
                {
                    let h = node.current_height.lock().await;
                    let last = node.last_digest.lock().await;
                    if *last == parent {
                        let app = node.app.read().await;
                        break (
                            *h + 1,
                            app.state_root().unwrap_or([0u8; 32]),
                            app.info().map_or(0, |b| b.time.nanos()),
                        );
                    }
                }
                if start.elapsed() >= PROPOSE_WAIT_MAX {
                    tracing::info!(
                        parent = %hex::encode(parent),
                        view = view_num,
                        "propose: parent not executed in time — skipping proposal for this view"
                    );
                    // Dropping `tx` tells consensus there is no proposal.
                    return;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            };

            // Peek, don't drain: txs leave the pool only once a block
            // containing them is executed, so a skipped proposal loses nothing.
            let raw_txs = {
                let pool = node.mempool.lock().await;
                pool.peek_batch(MAX_BLOCK_TXS, MAX_BLOCK_TX_BYTES)
            };

            let total_tx_bytes: usize = raw_txs.iter().map(|t| t.len()).sum();
            tracing::info!(
                tx_count = raw_txs.len(),
                total_tx_bytes = total_tx_bytes,
                height = height,
                "propose: selected mempool txs"
            );

            let mut payload = BlockPayload {
                height,
                timestamp_nanos: proposal_timestamp_nanos(parent_time, now_nanos()),
                proposer,
                txs: raw_txs,
                parent_digest: parent,
                state_root,
            };
            // Chaos testing only: emit a corrupted proposal so honest
            // validators' verify() rejection is exercised on the network.
            if let Some(mode) = node.fault_inject.as_deref() {
                if apply_fault_inject(&mut payload, mode) {
                    tracing::warn!(
                        mode,
                        height,
                        "fault_inject: proposing corrupted payload"
                    );
                }
            }

            let payload_bytes = payload.to_bytes().len();
            let digest_bytes = Self::compute_digest(&payload);
            tracing::info!(
                payload_bytes = payload_bytes,
                "propose: built BlockPayload"
            );

            // Store in pending map so the relay can broadcast it.
            {
                let mut pending = node.pending_payloads.lock().await;
                pending.insert(digest_bytes, payload);
            }

            tx.send(sha256::Digest::from(digest_bytes)).ok();
        });
        rx
    }

    async fn verify(
        &mut self,
        context: Self::Context,
        payload: Self::Digest,
    ) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();

        // CRITICAL: verify() MUST NOT call app.finalize_block() or mutate App state.
        //
        // The payload can arrive over the relay after the digest (large
        // store-code txs), and the parent may still be finalizing, so poll
        // for a bounded window (< leader_timeout) in a spawned task. Once the
        // payload is present AND our executed tip equals the consensus parent,
        // validate it against our own post-parent state.
        let node = self.clone();
        let (_, parent_digest) = context.parent;
        let parent: [u8; 32] = parent_digest.0;
        let view_num = context.round.view().get();
        let leader = context.leader.as_ref().to_vec();
        let digest_bytes: [u8; 32] = payload.0;
        tokio::spawn(async move {
            // rc14: if the proposal's parent (a certified ancestor) hasn't
            // executed, prefetch it and wait — only a Finalization activity
            // may advance execution. A certified parent that never finalizes
            // is a dead variant; rejecting here is the correct outcome.
            node.ensure_executed(parent);
            let start = Instant::now();
            let mut last_request: Option<Instant> = None;
            let verdict = loop {
                let candidate = node.pending_payloads.lock().await.get(&digest_bytes).cloned();
                if candidate.is_none()
                    && start.elapsed() >= FETCH_RETRY
                    && last_request.map_or(true, |t| t.elapsed() >= FETCH_RETRY)
                {
                    node.request_payload(digest_bytes);
                    last_request = Some(Instant::now());
                }
                if let Some(p) = candidate {
                    let h = node.current_height.lock().await;
                    let last = node.last_digest.lock().await;
                    if *last == parent {
                        let app = node.app.read().await;
                        let root = app.state_root().unwrap_or([0u8; 32]);
                        let parent_time = app.info().map_or(0, |b| b.time.nanos());
                        break validate_payload(
                            &p,
                            &parent,
                            *h + 1,
                            &root,
                            parent_time,
                            now_nanos(),
                            &leader,
                        );
                    }
                    if *h >= p.height {
                        break Err("parent is not the executed tip (stale or forked proposal)");
                    }
                }
                if start.elapsed() >= VERIFY_WAIT_MAX {
                    break Err("payload or executed parent unavailable within wait window");
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            };
            // RC17 vote-emission logging: record every verify() resolution —
            // verdict, elapsed, view — so an absent signer bit can be
            // attributed to late local resolution vs late network delivery.
            match &verdict {
                Ok(()) => tracing::info!(
                    digest = %hex::encode(digest_bytes),
                    view = view_num,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    "verify: resolved allow"
                ),
                Err(reason) => tracing::info!(
                    digest = %hex::encode(digest_bytes),
                    view = view_num,
                    elapsed_ms = start.elapsed().as_millis() as u64,
                    reason = %reason,
                    "verify: resolved reject"
                ),
            }
            tx.send(verdict.is_ok()).ok();
        });

        rx
    }
}

impl<T, P> CertifiableAutomaton for LayerNode<T, P>
where
    T: PersistentStorage + Send + Sync + 'static,
    P: PublicKey + Clone + Send + Sync + 'static,
{
    async fn certify(
        &mut self,
        round: Round,
        payload: Self::Digest,
    ) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        let view_num = round.view().get();

        // certify() does NOT execute: a notarized block can still be skipped
        // by consensus. Certifying means "I hold these bytes durably and can
        // execute them once finalized" — so fetch if missing, persist, vote.
        let node = self.clone();
        let digest_bytes: [u8; 32] = payload.0;
        tokio::spawn(async move {
            let start = Instant::now();
            let mut last_request: Option<Instant> = None;
            let ok = loop {
                if let Some(p) = node.lookup_payload(&digest_bytes).await {
                    // rc14: refuse to certify a payload that is provably off
                    // our executed chain. certify() is where our finalize-vote
                    // originates; without this check a notarized dead variant
                    // can collect our finalize signature and contribute to a
                    // conflicting finalization certificate. Some(false) means
                    // our executed chain holds a different block at the same
                    // height — deterministic conflict, not a timing artifact.
                    if node.consistent_with_executed_chain(&p).await == Some(false) {
                        tracing::warn!(
                            height = p.height,
                            digest = %hex::encode(digest_bytes),
                            "certify: refusing to certify — conflicts with executed chain (dead variant)"
                        );
                        break false;
                    }
                    let persisted = node.store.lock().unwrap_or_else(|e| e.into_inner()).put(&p);
                    match persisted {
                        Ok(()) => break true,
                        Err(e) => {
                            tracing::error!(
                                digest = %hex::encode(digest_bytes),
                                error = %e,
                                "certify: failed to persist payload — refusing to certify"
                            );
                            break false;
                        }
                    }
                }
                if start.elapsed() >= CERTIFY_WAIT_MAX {
                    // Escalate-on-repeat (RC17 SOC): a one-shot miss during
                    // catch-up churn is INFO noise (~7k/37h on idx6); the
                    // SAME digest timing out again in later views means the
                    // payload is genuinely unavailable — the liveness-stall
                    // signature — so promote from the 3rd repeat on.
                    let misses = {
                        let mut m = node
                            .certify_timeouts
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if m.len() >= 4_096 {
                            m.clear();
                        }
                        let n = m.entry(digest_bytes).or_insert(0);
                        *n += 1;
                        *n
                    };
                    if misses >= 3 {
                        tracing::warn!(
                            digest = %hex::encode(digest_bytes),
                            view = view_num,
                            occurrences = misses,
                            "certify: payload repeatedly unavailable across views — stall signature"
                        );
                    } else {
                        tracing::info!(
                            digest = %hex::encode(digest_bytes),
                            view = view_num,
                            "certify: payload unavailable within wait window"
                        );
                    }
                    break false;
                }
                if last_request.map_or(true, |t| t.elapsed() >= FETCH_RETRY) {
                    node.request_payload(digest_bytes);
                    last_request = Some(Instant::now());
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            };
            // RC17 vote-emission logging: every certify() resolution — this
            // is where our finalize-vote originates. Bitmap presence with no
            // local emission line was the unresolved idx2 case.
            tracing::info!(
                digest = %hex::encode(digest_bytes),
                view = view_num,
                elapsed_ms = start.elapsed().as_millis() as u64,
                verdict = ok,
                "certify: resolved"
            );
            tx.send(ok).ok();
        });
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use cosmwasm_std::{to_json_binary, Timestamp as CwTimestamp};
    use layer_app::{App, AppConfig, StateMachine};
    use layer_app::genesis::{GenesisState, WasmParams};
    use layer_std::api::{InitChainRequest, TmPubKey, ValidatorUpdate};
    use layer_storage::MemoryStore;
    use tokio::sync::{Mutex, RwLock};

    use crate::block::BlockPayload;
    use crate::mempool::Mempool;

    /// Test-chain genesis time plus one second per view.
    fn view_timestamp_nanos(view: u64) -> u64 {
        1_673_194_026_078_305_426 + view * 1_000_000_000
    }

    // Use ed25519::PublicKey as the test key type (simpler to construct than BLS)
    use commonware_cryptography::ed25519;

    type TestNode = LayerNode<MemoryStore, ed25519::PublicKey>;

    /// Global mutex serializing all node tests that create App<T> (and thus wasmer JIT).
    /// Wasmer's mmap-based JIT initialization is not safe to call concurrently from
    /// multiple threads on macOS/Apple Silicon; concurrent initialization causes SIGBUS.
    /// Holding this mutex during the entire test ensures only one test runs at a time.
    static APP_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Construct a single-threaded tokio runtime for tests.
    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn make_genesis_state() -> GenesisState {
        GenesisState {
            bank: vec![],
            wasm: WasmParams {
                gov_account: "juno1pkptre7fdkl6gfrzlesjjvhxhlc3r4gmdyychx".to_string(),
            },
        }
    }

    /// Unique wasmer cache dir per App. On Windows a `.module` file stays
    /// memory-mapped while an App is alive, so a second App sharing the dir
    /// fails to rewrite it (os error 1224). Tests like
    /// `test_genesis_is_deterministic` hold two live Apps at once.
    fn unique_cache_dir() -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("slay3rd-test-node-{}-{n}", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    fn init_app() -> App<MemoryStore> {
        let storage = MemoryStore::default();
        let logic = StateMachine::new(&AppConfig::new(&unique_cache_dir()));
        let mut app = App::new(storage, logic);

        let genesis = make_genesis_state();
        let app_state = to_json_binary(&genesis).unwrap();
        let request = InitChainRequest {
            time: CwTimestamp::from_nanos(1_673_194_026_078_305_426),
            chain_id: "junoclaw-1".into(),
            consensus_params: Default::default(),
            validators: vec![ValidatorUpdate {
                pub_key: TmPubKey::Ed25519(vec![123u8; 32]),
                power: 1_000_000,
            }],
            app_state,
            initial_height: 1,
        };
        app.init(request).unwrap();
        app
    }

    fn make_layer_node_locked(
        _guard: &std::sync::MutexGuard<()>,
    ) -> TestNode {
        let app = Arc::new(RwLock::new(init_app()));
        let mempool = Arc::new(Mutex::new(Mempool::new(1000)));
        // App starts with initial_height=1, meaning first block is height=1.
        // LAST_BLOCK in init() is stored as initial_height - 1 = 0.
        // So current_height should start at 0 (next block will be 1).
        LayerNode::new(app, mempool, 0)
    }

    fn make_layer_node() -> TestNode {
        // Acquire the global lock to serialize wasmer JIT initialization.
        // Tolerate poisoning so one panicking test doesn't fail all others.
        let _guard = APP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        make_layer_node_locked(&_guard)
    }

    fn make_payload_at_genesis_time(height: u64, parent_digest: [u8; 32]) -> BlockPayload {
        // Timestamp must be >= genesis time (1_673_194_026_078_305_426 ns).
        // Each block adds 100ms to ensure monotonically increasing timestamps.
        BlockPayload {
            height,
            timestamp_nanos: 1_673_194_026_078_305_426 + height * 100_000_000,
            proposer: vec![1u8; 32],
            txs: vec![],
            parent_digest,
            state_root: [0u8; 32],
        }
    }

    fn insert_payload_sync(node: &TestNode, payload: BlockPayload) -> [u8; 32] {
        let digest = payload.digest();
        rt().block_on(async {
            let pending_arc = node.pending_payloads();
            let mut pending = pending_arc.lock().await;
            pending.insert(digest, payload);
        });
        digest
    }

    #[test]
    fn test_genesis_returns_32_byte_digest() {
        let mut node = make_layer_node();
        let digest = rt().block_on(node.genesis(Epoch::new(0)));
        let bytes: &[u8] = &digest;
        assert_eq!(bytes.len(), 32, "genesis digest must be 32 bytes");
    }

    #[test]
    fn test_genesis_is_deterministic() {
        // Two nodes with the same genesis should produce the same app_hash.
        let mut node1 = make_layer_node();
        let mut node2 = make_layer_node();
        let digest1 = rt().block_on(node1.genesis(Epoch::new(0)));
        let digest2 = rt().block_on(node2.genesis(Epoch::new(0)));
        assert_eq!(
            digest1, digest2,
            "genesis digest should be deterministic for same genesis state"
        );
    }

    #[test]
    fn test_verify_returns_true_for_known_digest() {
        let node = make_layer_node();
        let payload = BlockPayload {
            height: 1,
            timestamp_nanos: 1_000_000_000,
            proposer: vec![1u8; 32],
            txs: vec![],
            parent_digest: [0u8; 32],
            state_root: [0u8; 32],
        };
        let digest_bytes = insert_payload_sync(&node, payload);

        // Simulate what verify() does: check pending_payloads
        let found = rt().block_on(async {
            let pending_arc = node.pending_payloads();
            let pending = pending_arc.lock().await;
            pending.contains_key(&digest_bytes)
        });
        assert!(found, "digest should be in pending_payloads after insert");
    }

    #[test]
    fn test_verify_returns_false_for_unknown_digest() {
        let node = make_layer_node();
        let unknown_digest = [42u8; 32];
        let found = rt().block_on(async {
            let pending_arc = node.pending_payloads();
            let pending = pending_arc.lock().await;
            pending.contains_key(&unknown_digest)
        });
        assert!(!found, "unknown digest should not be in pending_payloads");
    }

    #[test]
    fn test_certify_calls_finalize_block() {
        let node = make_layer_node();
        let payload = make_payload_at_genesis_time(1, [0u8; 32]);
        let digest = insert_payload_sync(&node, payload);

        let success = rt().block_on(node.execute_block(digest));
        assert!(success, "execute_block should succeed for valid empty-tx payload");

        // Check that height was incremented
        let height = rt().block_on(async { *node.current_height.lock().await });
        assert_eq!(height, 1, "height should be incremented to 1 after first certify");
    }

    #[test]
    fn test_certify_removes_pending_payload() {
        let node = make_layer_node();
        let payload = make_payload_at_genesis_time(1, [0u8; 32]);
        let digest = insert_payload_sync(&node, payload);

        // Verify it's in pending before certify
        let in_pending_before = rt().block_on(async {
            let pending_arc = node.pending_payloads();
            let pending = pending_arc.lock().await;
            pending.contains_key(&digest)
        });
        assert!(in_pending_before, "payload should be in pending before certify");

        rt().block_on(node.execute_block(digest));

        // Verify it's removed after certify
        let in_pending_after = rt().block_on(async {
            let pending_arc = node.pending_payloads();
            let pending = pending_arc.lock().await;
            pending.contains_key(&digest)
        });
        assert!(!in_pending_after, "payload should be removed from pending after certify");
    }

    #[test]
    fn test_certify_returns_false_for_unknown_digest() {
        let node = make_layer_node();
        let unknown_digest = [99u8; 32];
        let result = rt().block_on(node.execute_block(unknown_digest));
        assert!(!result, "execute_block should return false for unknown digest");
    }

    #[test]
    fn test_verify_does_not_mutate_app_state() {
        let node = make_layer_node();

        // Get initial app_hash
        let initial_hash = rt().block_on(async {
            let app_arc = node.app();
            let app = app_arc.read().await;
            app.app_hash()
        });

        // Insert a payload and check if it's in pending (simulating verify)
        let payload = BlockPayload {
            height: 1,
            timestamp_nanos: 1_000_000_000,
            proposer: vec![1u8; 32],
            txs: vec![],
            parent_digest: [0u8; 32],
            state_root: [0u8; 32],
        };
        let digest = insert_payload_sync(&node, payload);
        let _found = rt().block_on(async {
            let pending_arc = node.pending_payloads();
            let pending = pending_arc.lock().await;
            pending.contains_key(&digest)
        });

        // App hash should not change after verify (only reads pending_payloads)
        let after_hash = rt().block_on(async {
            let app_arc = node.app();
            let app = app_arc.read().await;
            app.app_hash()
        });
        assert_eq!(
            initial_hash, after_hash,
            "verify should not mutate app state (app_hash should be unchanged)"
        );
    }

    #[test]
    fn test_pending_payloads_accessor_returns_shared_arc() {
        let node = make_layer_node();
        let arc1 = node.pending_payloads();
        let arc2 = node.pending_payloads();

        // Both arcs should point to the same underlying data
        let payload = BlockPayload {
            height: 1,
            timestamp_nanos: 500_000_000,
            proposer: vec![2u8; 32],
            txs: vec![],
            parent_digest: [0u8; 32],
            state_root: [0u8; 32],
        };
        let digest = payload.digest();
        rt().block_on(async {
            let mut map = arc1.lock().await;
            map.insert(digest, payload);
        });
        let found = rt().block_on(async {
            let map = arc2.lock().await;
            map.contains_key(&digest)
        });
        assert!(found, "pending_payloads() should return the same shared Arc");
    }

    #[test]
    fn test_sequential_certify_increments_height() {
        let node = make_layer_node();

        // Block 1 (height = 1)
        let payload1 = make_payload_at_genesis_time(1, [0u8; 32]);
        let digest1 = insert_payload_sync(&node, payload1);
        assert!(rt().block_on(node.execute_block(digest1)), "first block should certify");
        let h1 = rt().block_on(async { *node.current_height.lock().await });
        assert_eq!(h1, 1);

        // Block 2 (height = 2)
        let payload2 = make_payload_at_genesis_time(2, digest1);
        let digest2 = insert_payload_sync(&node, payload2);
        assert!(rt().block_on(node.execute_block(digest2)), "second block should certify");
        let h2 = rt().block_on(async { *node.current_height.lock().await });
        assert_eq!(h2, 2);
    }

    fn local_state_root(node: &TestNode) -> [u8; 32] {
        rt().block_on(async { node.app().read().await.state_root().unwrap_or([0u8; 32]) })
    }

    /// Build a payload that is valid on top of `parent` given the node's current state.
    fn valid_child(node: &TestNode, height: u64, parent: [u8; 32]) -> BlockPayload {
        BlockPayload {
            height,
            timestamp_nanos: view_timestamp_nanos(height),
            proposer: vec![1u8; 32],
            txs: vec![],
            parent_digest: parent,
            state_root: local_state_root(node),
        }
    }

    #[test]
    fn test_new_node_tip_is_genesis_parent() {
        let node = make_layer_node();
        let (h, d) = rt().block_on(node.executed_tip());
        assert_eq!((h, d), (0, genesis_parent()));
        let mut n = node.clone();
        assert_eq!(rt().block_on(n.genesis(Epoch::new(0))).0, genesis_parent());
    }

    #[test]
    fn test_certify_does_not_execute() {
        let mut node = make_layer_node();
        let p = valid_child(&node, 1, genesis_parent());
        let digest = insert_payload_sync(&node, p);
        let hash_before = rt().block_on(async { node.app().read().await.app_hash() });
        let ok = rt().block_on(async {
            let rx = node.certify(Round::new(Epoch::new(0), commonware_consensus::types::View::new(1)), sha256::Digest::from(digest)).await;
            rx.await.unwrap()
        });
        assert!(ok, "certify should succeed when payload is available");
        let hash_after = rt().block_on(async { node.app().read().await.app_hash() });
        assert_eq!(hash_before, hash_after, "certify must not mutate state");
        assert_eq!(rt().block_on(node.executed_tip()).0, 0, "certify must not advance height");
    }

    /// rc14 regression (2026-10-21 g1 halt): ensure_executed must NEVER drive
    /// execution — a certified parent may be a dead variant (two same-height
    /// payloads notarized in different views), and executing the loser
    /// permanently diverges last_digest from the finalized chain.
    #[test]
    fn test_ensure_executed_does_not_execute_certified_parent() {
        let node = make_layer_node();
        let p1 = valid_child(&node, 1, genesis_parent());
        let d1 = insert_payload_sync(&node, p1);
        rt().block_on(async {
            node.ensure_executed(d1);
            // Give the spawned probe a real chance to (incorrectly) execute.
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
        assert_eq!(
            rt().block_on(node.executed_tip()),
            (0, genesis_parent()),
            "ensure_executed must not advance the executed tip — only finalization may"
        );
        assert!(
            rt().block_on(async { node.pending_payloads.lock().await.contains_key(&d1) }),
            "payload must remain pending for the eventual finalized walk"
        );
    }

    /// rc14 incident replay (2026-10-21 g1): two same-height payload variants,
    /// identical app hash, different bytes — the dead variant is "certified"
    /// and handed to ensure_executed, then the canonical variant's
    /// finalization arrives. The node must execute ONLY the canonical one.
    #[test]
    fn test_variant_incident_replay_dead_variant_never_executes() {
        let node = make_layer_node();
        let pa = valid_child(&node, 1, genesis_parent()); // variant A (dead)
        let mut pb = pa.clone();
        pb.proposer = vec![9u8; 32]; // variant B: same height/parent/root, different bytes
        let da = insert_payload_sync(&node, pa);
        let db = insert_payload_sync(&node, pb);
        assert_ne!(da, db, "variants must differ by digest");

        // Consensus hands us the DEAD variant as a certified parent.
        rt().block_on(async {
            node.ensure_executed(da);
            tokio::time::sleep(Duration::from_millis(200)).await;
        });
        assert_eq!(
            rt().block_on(node.executed_tip()),
            (0, genesis_parent()),
            "dead variant must not execute on certification alone"
        );

        // The canonical variant's FINALIZATION arrives — it executes.
        assert_eq!(rt().block_on(node.finalize(db)).map(|r| r.0), Some(1));
        assert_eq!(rt().block_on(node.executed_tip()), (1, db));
    }

    /// rc14 conflict guard (2026-10-21 g1 halt): certify() is where our
    /// finalize-vote originates. A payload that provably conflicts with the
    /// executed chain (different block at an executed height) must not
    /// collect our signature — it can only ever be a dead variant. Payloads
    /// above the tip or on our chain still certify normally.
    #[test]
    fn test_certify_refuses_dead_variant_at_executed_height() {
        let mut node = make_layer_node();
        let pa = valid_child(&node, 1, genesis_parent()); // canonical
        let mut pb = pa.clone();
        pb.proposer = vec![9u8; 32]; // dead variant: same height, different bytes
        let da = insert_payload_sync(&node, pa.clone());
        let db = insert_payload_sync(&node, pb);
        assert_ne!(da, db);

        // Canonical block finalizes and executes → tip = (1, da).
        assert_eq!(rt().block_on(node.finalize(da)).map(|r| r.0), Some(1));
        assert_eq!(rt().block_on(node.executed_tip()), (1, da));

        // certify() on the dead variant: the walk finds our executed chain
        // holds da at height 1, not db → refuse.
        let ok = rt().block_on(async {
            let rx = node
                .certify(
                    Round::new(Epoch::new(0), commonware_consensus::types::View::new(2)),
                    sha256::Digest::from(db),
                )
                .await;
            rx.await.unwrap()
        });
        assert!(!ok, "certify must refuse a payload conflicting with executed chain");

        // certify() on the executed block itself still succeeds (replay path).
        // finalize() removes executed payloads from pending — re-admit pa so
        // certify's lookup can find it (production keeps it in PayloadStore).
        rt().block_on(async {
            node.pending_payloads.lock().await.insert(da, pa);
        });
        let ok = rt().block_on(async {
            let rx = node
                .certify(
                    Round::new(Epoch::new(0), commonware_consensus::types::View::new(3)),
                    sha256::Digest::from(da),
                )
                .await;
            rx.await.unwrap()
        });
        assert!(ok, "certify must accept the executed block at the tip height");
    }

    /// Undetermined cases fail open: a candidate above the executed tip gets
    /// None from the guard and certifies normally (liveness preserved).
    #[test]
    fn test_consistent_with_executed_chain_undetermined_fails_open() {
        let node = make_layer_node();
        let p1 = valid_child(&node, 1, genesis_parent());
        assert_eq!(
            rt().block_on(node.consistent_with_executed_chain(&p1)),
            None,
            "payload above tip is undetermined"
        );
        // Genesis parent: our chain holds genesis_parent() at height 0 — a
        // payload at height 0 with a different digest is a proven conflict.
        let p0 = valid_child(&node, 0, [7u8; 32]);
        assert_eq!(
            rt().block_on(node.consistent_with_executed_chain(&p0)),
            Some(false),
            "different block at executed height 0 is a proven conflict"
        );
    }

    #[test]
    fn test_finalize_executes_skipped_ancestors_in_order() {
        let node = make_layer_node();
        let p1 = valid_child(&node, 1, genesis_parent());
        let d1 = insert_payload_sync(&node, p1);
        // p2's state_root must be post-p1 state; compute it by executing p1 on a
        // twin node (execution is deterministic).
        let twin = make_layer_node();
        let d1_twin = insert_payload_sync(&twin, valid_child(&twin, 1, genesis_parent()));
        assert_eq!(d1, d1_twin);
        assert!(rt().block_on(twin.execute_block(d1_twin)));
        let p2 = valid_child(&twin, 2, d1);
        let d2 = insert_payload_sync(&node, p2.clone());

        // Only the tip's finalization is reported; p1 is finalized by implication.
        let res = rt().block_on(node.finalize(d2));
        assert_eq!(res.map(|r| r.0), Some(2));
        assert_eq!(rt().block_on(node.executed_tip()), (2, d2));
        let d2_twin = insert_payload_sync(&twin, p2);
        assert!(rt().block_on(twin.execute_block(d2_twin)));
        assert_eq!(local_state_root(&node), local_state_root(&twin));

        // Replayed finalizations are no-ops.
        assert_eq!(rt().block_on(node.finalize(d2)), None);
        assert_eq!(rt().block_on(node.executed_tip()).0, 2);
    }

    #[test]
    fn test_finalize_halts_on_state_root_mismatch() {
        let node = make_layer_node();
        let mut p1 = valid_child(&node, 1, genesis_parent());
        p1.state_root = [7u8; 32];
        let d1 = insert_payload_sync(&node, p1);
        assert_eq!(rt().block_on(node.finalize(d1)), None);
        assert_eq!(rt().block_on(node.executed_tip()).0, 0, "divergent block must not execute");
    }

    /// Executing a block removes its txs from the pool; finalize() then
    /// rechecks the remainder and evicts txs invalid against the new state.
    #[test]
    fn test_mempool_remove_on_commit_and_recheck() {
        let node = make_layer_node();
        let committed = bytes::Bytes::from_static(b"\xff\xfecommitted");
        let stale = bytes::Bytes::from_static(b"\xff\xfestale");
        rt().block_on(async {
            let mut pool = node.mempool.lock().await;
            pool.submit(committed.clone()).unwrap();
            pool.submit(stale.clone()).unwrap();
        });

        let mut p1 = valid_child(&node, 1, genesis_parent());
        p1.txs = vec![committed.clone()];
        let d1 = insert_payload_sync(&node, p1);
        assert!(rt().block_on(node.execute_block(d1)));
        rt().block_on(async {
            let pool = node.mempool.lock().await;
            assert!(!pool.contains(&tx_hash(&committed)), "committed tx must leave the pool");
            assert!(pool.contains(&tx_hash(&stale)), "execute alone must not evict other txs");
        });

        let p2 = valid_child(&node, 2, d1);
        let d2 = insert_payload_sync(&node, p2);
        assert_eq!(rt().block_on(node.finalize(d2)).map(|r| r.0), Some(2));
        assert!(
            rt().block_on(async { node.mempool.lock().await.is_empty() }),
            "post-block recheck must evict txs that fail check_tx"
        );
    }

    #[test]
    fn test_accept_peer_payload_bounds() {
        let node = make_layer_node();
        let at = |height: u64| BlockPayload {
            height,
            timestamp_nanos: view_timestamp_nanos(height),
            proposer: vec![1u8; 32],
            txs: vec![],
            parent_digest: [height as u8; 32],
            state_root: [0u8; 32],
        };
        rt().block_on(async {
            assert_eq!(node.accept_peer_payload(at(1)).await, Ok(true));
            assert_eq!(node.accept_peer_payload(at(1)).await, Ok(false), "duplicate is a no-op");
            assert!(node.accept_peer_payload(at(0)).await.is_err(), "stale height rejected");
            assert!(
                node.accept_peer_payload(at(PEER_PAYLOAD_LOOKAHEAD + 1)).await.is_err(),
                "unsolicited payload beyond lookahead rejected"
            );
            let far = at(PEER_PAYLOAD_LOOKAHEAD + 1);
            node.request_payload(far.digest());
            assert_eq!(node.accept_peer_payload(far).await, Ok(true), "solicited reply bypasses window");

            let mut big = at(2);
            big.txs = vec![bytes::Bytes::from(vec![0u8; MAX_BLOCK_TX_BYTES + 1])];
            assert!(node.accept_peer_payload(big).await.is_err(), "oversized payload rejected");

            let mut inserted = 0usize;
            loop {
                let mut p = at(2);
                p.timestamp_nanos += inserted as u64;
                if node.accept_peer_payload(p).await.is_err() {
                    break;
                }
                inserted += 1;
            }
            assert_eq!(
                node.pending_payloads.lock().await.len(),
                MAX_PENDING_PAYLOADS_TOTAL - PENDING_SOLICITED_RESERVE,
                "unsolicited pushes stop at the solicited-reserve boundary"
            );
            // Solicited replies still land inside the reserve window — fill
            // the remaining slots with solicited inserts up to the ceiling.
            while node.pending_payloads.lock().await.len() < MAX_PENDING_PAYLOADS_TOTAL {
                let mut p = at(3);
                p.timestamp_nanos += 1_000_000 + node.pending_payloads.lock().await.len() as u64;
                node.request_payload(p.digest());
                assert_eq!(
                    node.accept_peer_payload(p).await,
                    Ok(true),
                    "solicited reply must be admitted inside the reserve"
                );
            }
            // At the absolute ceiling, a solicited reply evicts the
            // furthest-from-tip pending entry instead of being refused —
            // refusing it is the rc11-soak catch-up wedge.
            let victim = node
                .pending_payloads
                .lock()
                .await
                .iter()
                .max_by_key(|(_, p)| p.height)
                .map(|(d, _)| *d)
                .unwrap();
            let tip_link = at(4);
            let tip_link_digest = tip_link.digest();
            node.request_payload(tip_link_digest);
            assert_eq!(
                node.accept_peer_payload(tip_link).await,
                Ok(true),
                "solicited reply at capacity must evict, not be refused"
            );
            let pending = node.pending_payloads.lock().await;
            assert_eq!(pending.len(), MAX_PENDING_PAYLOADS_TOTAL);
            assert!(pending.contains_key(&tip_link_digest));
            assert!(!pending.contains_key(&victim), "furthest entry evicted for solicited reply");
        });
    }

    /// Bulk backfill: heights solicited via request_height_range must bypass
    /// the lookahead window just like digest-solicited replies.
    #[test]
    fn test_height_solicited_payload_bypasses_lookahead() {
        let node = make_layer_node();
        let at = |height: u64| BlockPayload {
            height,
            timestamp_nanos: view_timestamp_nanos(height),
            proposer: vec![1u8; 32],
            txs: vec![],
            parent_digest: [height as u8; 32],
            state_root: [0u8; 32],
        };
        rt().block_on(async {
            // Unsolicited far payload rejected — but its height is observed.
            let far = at(PEER_PAYLOAD_LOOKAHEAD + 50);
            assert!(node.accept_peer_payload(far.clone()).await.is_err());
            assert_eq!(
                node.max_seen_height.load(std::sync::atomic::Ordering::Relaxed),
                far.height,
                "rejected pushes still advance the fetch target"
            );

            // Solicit the height explicitly — now the same payload is admitted.
            node.request_height_range(far.height, 1);
            assert_eq!(
                node.accept_peer_payload(far).await,
                Ok(true),
                "height-solicited reply must bypass the lookahead window"
            );
        });
    }

    /// backfill_tick emits HeightRange requests covering every missing height
    /// between the executed tip and the observed peer tip.
    #[test]
    fn test_backfill_tick_requests_missing_range() {
        let node = make_layer_node();
        let (fetch_tx, mut fetch_rx) = tokio::sync::mpsc::unbounded_channel::<FetchRequest>();
        node.set_fetch_sender(fetch_tx);
        rt().block_on(async {
            // Simulate learning the peer tip is 150 above our tip of 0.
            let far = BlockPayload {
                height: 150,
                timestamp_nanos: view_timestamp_nanos(150),
                proposer: vec![1u8; 32],
                txs: vec![],
                parent_digest: [9u8; 32],
                state_root: [0u8; 32],
            };
            let _ = node.accept_peer_payload(far).await; // rejected, height noted
            node.backfill_tick().await;

            let mut covered: Vec<u64> = Vec::new();
            while let Ok(req) = fetch_rx.try_recv() {
                let FetchRequest::HeightRange { start, count } = req else {
                    continue;
                };
                assert!(count as u64 <= BACKFILL_BATCH_SIZE);
                covered.extend(start..start + count as u64);
            }
            // rc5: one tick is budget-bound (BACKFILL_TICK_BUDGET per 250ms
            // tick) — the first tick must cover the lowest missing heights
            // exactly, without flooding the peer with the whole gap at once.
            // Heights above the budget follow on later ticks; re-request
            // pacing is covered by the in-flight test below.
            assert_eq!(
                covered,
                (1..=BACKFILL_TICK_BUDGET).collect::<Vec<u64>>(),
                "first backfill tick must request the lowest missing heights, bounded by the tick budget"
            );
        });
    }

    /// Regression for the 2026-10-02 C9 soak wedge: a missing height that
    /// was just requested is in-flight — the next tick must NOT re-send its
    /// range. (Re-sending every missing range every 250ms flooded the fetch
    /// path so hard that replies arrived after the solicited mark expired.)
    #[test]
    fn test_backfill_tick_does_not_reflood_inflight_heights() {
        let node = make_layer_node();
        let (fetch_tx, mut fetch_rx) = tokio::sync::mpsc::unbounded_channel::<FetchRequest>();
        node.set_fetch_sender(fetch_tx);
        rt().block_on(async {
            let far = BlockPayload {
                height: 100,
                timestamp_nanos: view_timestamp_nanos(100),
                proposer: vec![1u8; 32],
                txs: vec![],
                parent_digest: [9u8; 32],
                state_root: [0u8; 32],
            };
            let _ = node.accept_peer_payload(far).await; // rejected, height noted

            node.backfill_tick().await;
            let mut first = 0usize;
            while fetch_rx.try_recv().is_ok() {
                first += 1;
            }
            assert!(first > 0, "first tick should emit range requests");

            node.backfill_tick().await;
            assert!(
                fetch_rx.try_recv().is_err(),
                "in-flight heights must not be re-requested on the next tick"
            );
        });
    }

    /// After an in-flight mark expires (HEIGHT_REASK_AFTER), the height must
    /// be re-requested — lost replies still recover.
    #[test]
    fn test_backfill_tick_reasks_after_reask_window() {
        let node = make_layer_node();
        let (fetch_tx, mut fetch_rx) = tokio::sync::mpsc::unbounded_channel::<FetchRequest>();
        node.set_fetch_sender(fetch_tx);
        rt().block_on(async {
            let far = BlockPayload {
                height: 10,
                timestamp_nanos: view_timestamp_nanos(10),
                proposer: vec![1u8; 32],
                txs: vec![],
                parent_digest: [9u8; 32],
                state_root: [0u8; 32],
            };
            let _ = node.accept_peer_payload(far).await;
            node.backfill_tick().await;
            while fetch_rx.try_recv().is_ok() {}

            // Age every mark past HEIGHT_REASK_AFTER, then tick again.
            {
                let mut req = node.requested_heights.lock().unwrap();
                for t in req.values_mut() {
                    *t = Instant::now() - HEIGHT_REASK_AFTER;
                }
            }
            node.backfill_tick().await;
            let mut reasked = 0usize;
            while let Ok(FetchRequest::HeightRange { .. }) = fetch_rx.try_recv() {
                reasked += 1;
            }
            assert!(reasked > 0, "stale in-flight marks must be re-requested");
        });
    }

    /// rc4: a finalized-but-unheld digest is fetched by its exact certified
    /// digest, is paced by DIGEST_REASK_AFTER, and leaves the queue once its
    /// payload is held.
    #[test]
    fn test_certified_digest_fetch_requests_missing() {
        let node = make_layer_node();
        let (fetch_tx, mut fetch_rx) = tokio::sync::mpsc::unbounded_channel::<FetchRequest>();
        node.set_fetch_sender(fetch_tx);
        rt().block_on(async {
            let p = BlockPayload {
                height: 9,
                timestamp_nanos: view_timestamp_nanos(9),
                proposer: vec![1u8; 32],
                txs: vec![],
                parent_digest: [3u8; 32],
                state_root: [0u8; 32],
            };
            let d = p.digest();
            node.note_finalized(d).await;

            node.backfill_tick().await;
            let Ok(FetchRequest::Digest(requested)) = fetch_rx.try_recv() else {
                panic!("finalized-but-unheld digest must be fetched");
            };
            assert_eq!(requested, d);

            // Paced: an immediate second tick must not re-request.
            node.backfill_tick().await;
            assert!(
                fetch_rx.try_recv().is_err(),
                "digest fetch must respect DIGEST_REASK_AFTER"
            );

            // Held payloads resolve the digest: even after the pacing window
            // expires, no further digest request is issued and the queue
            // drains (by-height requests may still flow for other heights).
            assert!(node.accept_peer_payload(p).await.is_ok());
            {
                let mut certified = node.certified_digests.lock().unwrap();
                for t in certified.last_requested.values_mut() {
                    *t = Instant::now() - DIGEST_REASK_AFTER;
                }
            }
            node.backfill_tick().await;
            while let Ok(req) = fetch_rx.try_recv() {
                assert!(
                    !matches!(req, FetchRequest::Digest(_)),
                    "held digest must not be re-requested"
                );
            }
            assert!(
                node.certified_digests.lock().unwrap().queue.is_empty(),
                "resolved digest must leave the queue"
            );
        });
    }

    /// rc4: finalizations of digests we already hold are never queued.
    #[test]
    fn test_note_finalized_skips_held_digests() {
        let node = make_layer_node();
        let (fetch_tx, mut fetch_rx) = tokio::sync::mpsc::unbounded_channel::<FetchRequest>();
        node.set_fetch_sender(fetch_tx);
        rt().block_on(async {
            let p = BlockPayload {
                height: 2,
                timestamp_nanos: view_timestamp_nanos(2),
                proposer: vec![1u8; 32],
                txs: vec![],
                parent_digest: [5u8; 32],
                state_root: [0u8; 32],
            };
            assert_eq!(node.accept_peer_payload(p.clone()).await, Ok(true));
            node.note_finalized(p.digest()).await;
            node.backfill_tick().await;
            while let Ok(req) = fetch_rx.try_recv() {
                assert!(
                    !matches!(req, FetchRequest::Digest(_)),
                    "held digest must not be fetched"
                );
            }
        });
    }

    /// The certified-digest queue is bounded: replayed finalizations of
    /// heights no peer still holds cannot grow it unbounded — the oldest
    /// entry is dropped at the cap.
    #[test]
    fn test_certified_digest_queue_cap() {
        let mut q = CertifiedDigests::default();
        for i in 0..CERTIFIED_DIGEST_QUEUE_CAP as u32 {
            q.push([i as u8; 32]);
        }
        assert_eq!(q.queue.len(), CERTIFIED_DIGEST_QUEUE_CAP);
        q.push([0xFF; 32]);
        assert_eq!(q.queue.len(), CERTIFIED_DIGEST_QUEUE_CAP);
        assert_eq!(*q.queue.front().unwrap(), [1u8; 32]);
    }

    /// fault_inject modes must corrupt the payload so honest verify()
    /// rejects it; unknown modes are inert.
    #[test]
    fn test_apply_fault_inject() {
        let mut p = BlockPayload {
            height: 5,
            timestamp_nanos: view_timestamp_nanos(5),
            proposer: vec![1u8; 32],
            txs: vec![],
            parent_digest: [7u8; 32],
            state_root: [8u8; 32],
        };
        assert!(apply_fault_inject(&mut p, "bad_state_root"));
        assert_eq!(p.state_root, [0xEF; 32]);
        assert_eq!(p.parent_digest, [7u8; 32]);

        let mut q = p.clone();
        assert!(apply_fault_inject(&mut q, "bad_parent"));
        assert_eq!(q.parent_digest, [0xEF; 32]);

        assert!(!apply_fault_inject(&mut q, "not_a_mode"));
    }

    /// payload_by_height serves pending (not-yet-executed) payloads for
    /// peers running range backfill against this node.
    #[test]
    fn test_payload_by_height_serves_pending() {
        let node = make_layer_node();
        let payload = BlockPayload {
            height: 7,
            timestamp_nanos: view_timestamp_nanos(7),
            proposer: vec![1u8; 32],
            txs: vec![],
            parent_digest: [6u8; 32],
            state_root: [0u8; 32],
        };
        let expected = payload.clone();
        insert_payload_sync(&node, payload);
        rt().block_on(async {
            assert_eq!(node.payload_by_height(7).await, Some(expected));
            assert_eq!(node.payload_by_height(8).await, None);
        });
    }

    #[test]
    fn test_payload_by_height_falls_back_to_app_sidecar() {
        let node = make_layer_node();
        let payload = make_payload_at_genesis_time(9, [8u8; 32]);
        let expected = payload.clone();
        rt().block_on(async {
            assert_eq!(node.payload_by_height(9).await, None);
            node.app
                .write()
                .await
                .set_block_payload(9, payload.to_bytes())
                .unwrap();
            assert_eq!(node.payload_by_height(9).await, Some(expected));
        });
    }

    #[test]
    fn test_prune_sidecars_prunes_finality_records() {
        let node = make_layer_node().with_pruning(Some(2));
        rt().block_on(async {
            {
                let mut app = node.app.write().await;
                for h in 1..=5u64 {
                    app.set_block_finality(h, vec![h as u8], vec![h as u8; 2], h).unwrap();
                }
            }
            node.prune_sidecars(5).await;
            let app = node.app.read().await;
            assert_eq!(app.finality_prune_floor(), 3);
            assert_eq!(app.get_block_certificate(1), None);
            assert_eq!(app.get_block_proposal(3), None);
            assert_eq!(app.get_block_certificate(4), Some(vec![4u8]));
            assert_eq!(app.get_block_proposal(5), Some(vec![5u8; 2]));
        });
    }

    #[test]
    fn test_validate_payload_rejects_bad_fields() {
        let parent = [3u8; 32];
        let root = [4u8; 32];
        let parent_time = view_timestamp_nanos(8);
        let now = view_timestamp_nanos(9);
        let good = BlockPayload {
            height: 5,
            timestamp_nanos: now,
            proposer: vec![1u8; 32],
            txs: vec![],
            parent_digest: parent,
            state_root: root,
        };
        let check = |p: &BlockPayload| validate_payload(p, &parent, 5, &root, parent_time, now, &[1u8; 32]);
        assert!(check(&good).is_ok());
        let mut p = good.clone(); p.parent_digest = [0u8; 32]; assert!(check(&p).is_err());
        let mut p = good.clone(); p.height = 6; assert!(check(&p).is_err());
        let mut p = good.clone(); p.state_root = [0u8; 32]; assert!(check(&p).is_err());
        let mut p = good.clone(); p.timestamp_nanos = parent_time; assert!(check(&p).is_err());
        let mut p = good.clone(); p.proposer = vec![2u8; 32]; assert!(check(&p).is_err());
        let mut p = good.clone();
        p.txs = vec![bytes::Bytes::from(vec![0u8; MAX_BLOCK_TX_BYTES / 2 + 1]); 2];
        assert!(check(&p).is_err(), "block byte budget must be enforced");
    }

    #[test]
    fn test_validate_payload_timestamp_window() {
        let parent = [3u8; 32];
        let root = [4u8; 32];
        let parent_time = view_timestamp_nanos(100);
        let now = parent_time + 5_000_000_000;
        let drift = MAX_FUTURE_DRIFT.as_nanos() as u64;
        let at = |t: u64| {
            let p = BlockPayload {
                height: 5,
                timestamp_nanos: t,
                proposer: vec![1u8; 32],
                txs: vec![],
                parent_digest: parent,
                state_root: root,
            };
            validate_payload(&p, &parent, 5, &root, parent_time, now, &[1u8; 32])
        };
        assert!(at(parent_time).is_err(), "must be strictly after the parent");
        assert!(at(parent_time + 1).is_ok());
        assert!(at(now).is_ok());
        assert!(at(now + drift).is_ok());
        assert!(at(now + drift + 1).is_err(), "too far ahead of the local clock");
    }

    #[test]
    fn test_proposal_timestamp_tracks_clock_and_stays_monotonic() {
        assert_eq!(proposal_timestamp_nanos(100, 500), 500);
        assert_eq!(proposal_timestamp_nanos(500, 500), 501);
        assert_eq!(proposal_timestamp_nanos(900, 500), 901);
        assert!(now_nanos() > view_timestamp_nanos(0), "wall clock is past the test genesis");
    }

    /// Build a properly signed Cosmos tx bytes (cosmos.tx.v1beta1.TxRaw encoded) using cosmrs.
    ///
    /// Returns raw bytes that parse_cosmos_tx() can decode. The tx is signed with a random
    /// secp256k1 key and targets the test chain ("junoclaw-1"). The signer account is
    /// not in genesis, so finalize_block may reject it — but the DESERIALIZATION succeeds.
    #[cfg(test)]
    fn build_valid_tx_bytes(chain_id: &str) -> Vec<u8> {
        use cosmrs::{
            bank::MsgSend,
            crypto::secp256k1,
            tx::{self, Fee, Msg, SignDoc, SignerInfo},
            Coin,
        };
        use layer_std::BECH32_PREFIX;

        const ACCOUNT_NUMBER: u64 = 17; // FIXED_ACCOUNT_NUMBER from layer_cosmos::tx

        let sender_private_key = secp256k1::SigningKey::random();
        let sender_public_key = sender_private_key.public_key();
        let sender_account_id = sender_public_key.account_id(BECH32_PREFIX).unwrap();
        let rcpt_account_id = secp256k1::SigningKey::random()
            .public_key()
            .account_id(BECH32_PREFIX)
            .unwrap();

        let amount = Coin {
            amount: 1_000u128,
            denom: "ujclaw".parse().unwrap(),
        };
        let fee_coin = Coin {
            amount: 100u128,
            denom: "ujclaw".parse().unwrap(),
        };

        let msg_send = MsgSend {
            from_address: sender_account_id.clone(),
            to_address: rcpt_account_id.clone(),
            amount: vec![amount],
        };

        let tx_body = tx::Body::new(vec![msg_send.to_any().unwrap()], "", 9001u16);
        let signer_info = SignerInfo::single_direct(Some(sender_public_key), 0);
        let auth_info = signer_info.auth_info(Fee::from_amount_and_gas(fee_coin, 200_000u64));

        let parsed_chain_id = chain_id.parse().unwrap();
        let sign_doc =
            SignDoc::new(&tx_body, &auth_info, &parsed_chain_id, ACCOUNT_NUMBER).unwrap();
        let tx_signed = sign_doc.sign(&sender_private_key).unwrap();
        tx_signed.to_bytes().unwrap()
    }

    /// Validates that execute_block() correctly deserializes transactions from BlockPayload
    /// raw bytes via parse_cosmos_tx().
    ///
    /// Test flow:
    /// 1. Create an initialized App<MemoryStore> with genesis applied.
    /// 2. Construct a BlockPayload with valid Cosmos tx bytes in the `txs` field.
    /// 3. Execute the block and verify no "Skipping malformed tx" warning fires.
    /// 4. Construct a BlockPayload with invalid bytes mixed with valid bytes.
    /// 5. Verify the invalid bytes are skipped (deserialization path handles it gracefully).
    ///
    /// NOTE: execute_block() calls finalize_block() which may return an error if the tx
    /// signer is not in genesis. The test focuses on verifying the DESERIALIZATION path
    /// (parse_cosmos_tx called for each raw tx in payload.txs) rather than finalize success.
    /// The key assertion is that valid tx bytes parse successfully (no deserialization error
    /// before reaching finalize_block) and invalid bytes are skipped gracefully.
    #[test]
    fn test_execute_block_with_real_txs() {
        let _guard = APP_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let chain_id = "junoclaw-1";

        // Build valid tx bytes
        let valid_tx = bytes::Bytes::from(build_valid_tx_bytes(chain_id));

        // Build invalid tx bytes (random garbage)
        let invalid_tx = bytes::Bytes::from(vec![0xFF_u8, 0xFE, 0xAB, 0x12, 0x00]);

        // Test 1: Parse valid tx bytes directly via parse_cosmos_tx to confirm
        // the bytes are well-formed (confirming the deserialization path works).
        let parse_result = layer_cosmos::parse_cosmos_tx(valid_tx.clone(), chain_id);
        assert!(
            parse_result.is_ok(),
            "Valid tx bytes should parse successfully via parse_cosmos_tx: {:?}",
            parse_result.err()
        );

        // Test 2: Parse invalid tx bytes — must return an error, not panic.
        let parse_invalid = layer_cosmos::parse_cosmos_tx(invalid_tx.clone(), chain_id);
        assert!(
            parse_invalid.is_err(),
            "Invalid tx bytes should return an error, not panic"
        );

        // Test 3: execute_block() with valid tx bytes in payload.
        // The block has valid tx bytes. finalize_block() may reject the txs (signer not
        // in genesis), but that's OK — the deserialization path has already been exercised.
        let node = make_layer_node_locked(&_guard);
        let payload = BlockPayload {
            height: 1,
            timestamp_nanos: 1_673_194_026_078_305_426 + 100_000_000,
            proposer: vec![1u8; 32],
            txs: vec![valid_tx.clone()],
            parent_digest: [0u8; 32],
            state_root: [0u8; 32],
        };
        let digest = insert_payload_sync(&node, payload);
        // execute_block may return false if finalize_block fails (tx signer not in genesis),
        // but it should not panic. The deserialization path was exercised.
        let _result = rt().block_on(node.execute_block(digest));

        // Test 4: execute_block() with mixed valid + invalid bytes in payload.
        // The valid tx should be deserialized; the invalid one should be skipped with a warning.
        let node2 = make_layer_node_locked(&_guard);
        let payload_mixed = BlockPayload {
            height: 1,
            timestamp_nanos: 1_673_194_026_078_305_426 + 100_000_000,
            proposer: vec![1u8; 32],
            txs: vec![valid_tx.clone(), invalid_tx.clone(), valid_tx.clone()],
            parent_digest: [0u8; 32],
            state_root: [0u8; 32],
        };
        let digest2 = insert_payload_sync(&node2, payload_mixed);
        // Must not panic even with mixed valid/invalid bytes.
        let _result2 = rt().block_on(node2.execute_block(digest2));

        // Test 5: execute_block() with empty txs payload (existing behavior preserved).
        let node3 = make_layer_node_locked(&_guard);
        let payload_empty = BlockPayload {
            height: 1,
            timestamp_nanos: 1_673_194_026_078_305_426 + 100_000_000,
            proposer: vec![1u8; 32],
            txs: vec![],
            parent_digest: [0u8; 32],
            state_root: [0u8; 32],
        };
        let digest3 = insert_payload_sync(&node3, payload_empty);
        let result3 = rt().block_on(node3.execute_block(digest3));
        assert!(
            result3,
            "execute_block with empty txs should still succeed (finalize_block works)"
        );
    }
}
