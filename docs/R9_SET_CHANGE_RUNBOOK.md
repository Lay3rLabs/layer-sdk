# R9 — Validator Set-Change Runbook (devnet rehearsal)

Status: **PASS** — W0 form, W1 add (4→5), W2 remove (5→4), retired-seat
isolation all verified on the `r9` Docker devnet, 2026-10-14.

This document is the runbook for changing the JunoClaw validator set on a
running chain. The current consensus implementation has a **static validator
set at epoch 0** — there is no in-protocol rotation. A set change is a
coordinated halt + restart procedure with a fresh Phase-A ceremony. Everything
here was validated on the `r9` Docker devnet before it may be attempted on G2.

## What a set change actually is

- A **fresh threshold polynomial** is dealt over the *new* participant set
  (new `keys.json` per seat, new `validator_set` blob). Old shares are
  invalid afterward — the threshold key itself changes.
- `validator_index` = position in **sorted Ed25519 pubkey order**, NOT the
  seat/op number. Adding a seat reshuffles indices — always re-derive from
  `shared.json`, never hand-assign.
- Each seat's `[[peers]]` list, `validator_index`, and `p2p_address` come
  from the ceremony output. On devnet the container IP must match the
  `p2p_address` the keys were bound to.
- All seats halt together, swap keys+config, restart together.

## Why the naive path fails (observed in rehearsal)

Two failure modes were found before the fix:

1. **Keeping the old consensus journal → panic.** The journal is a single
   append-only file (`slay3r-<chain_id>` partition) mixing voter artifacts and
   marshal state, all indexed by **signer position**. After a set change the
   sorted Ed25519 order reshuffles, so replayed votes attribute to the wrong
   signers:
   `replaying notarize from another signer` → `Executor channel closed`.
2. **Wiping the journal → height-1 livelock (pre-fix).** Consensus restarted
   at view 1 against the *fixed* genesis parent while app state was already
   at the tip → `parent not executed in time` forever.

### The fix (app/slay3rd)

Two changes landed:

1. `Automaton::genesis()` no longer returns only the fixed genesis parent.
   With executed history it anchors on the **executed tip**, so a wiped
   journal resumes consensus at `tip+1` instead of proposing height 1.
2. **`restart_anchor` in node.toml** — the rehearsal exposed that a
   non-atomic halt leaves seats at *different* executed heights (4928 vs
   4929), so "my tip" is not a shared parent. The coordinator sets
   `restart_anchor = { height, digest }` to the **highest executed tip
   observed at halt** on EVERY seat. `genesis()` returns the anchor digest
   while a seat's tip is behind it; lagging seats fetch + execute the gap
   through the normal `ensure_executed`/`verify` path. A seat that ever
   executes past `anchor.height` ignores it. Remove the config after the
   transition.

## Rehearsal layout

```
r9-rehearsal/
  w0-ceremony.sh        # Phase-A ceremony: 4-seat baseline (IPs .10-.13)
  w1-ceremony.sh        # adds seat .14 — fresh n=5 threshold deal
  w2-ceremony.sh        # retires seat .13 — fresh n=4 threshold deal
  stage-window.sh wN    # maps ceremony output to ip-<octet>/ mounts
  docker-compose.yml    # 5 services; binds ./bins/slay3rd over the image
  bins/slay3rd          # patched node binary (built in-bookworm, see below)
```

Building the patched binary for the bookworm image (WSL binaries are
glibc-2.38 and will NOT run in the image):

```
docker run --rm -v <repo>:/src -v r9-target:/t -v r9-cargo:/root/.cargo \
  rust:1.97-bookworm bash -c "
    apt-get update -qq && apt-get install -y -qq clang g++ libclang-dev cmake &&
    cc -c -o /tmp/shim.o /src/docker/shim.c &&
    cd /src && CARGO_TARGET_DIR=/t RUSTFLAGS='-C link-arg=/tmp/shim.o' \
      cargo build --release --bin slay3rd --features rocksdb &&
    cp /t/release/slay3rd /src/r9-rehearsal/bins/slay3rd"
```

(The shim resolves `__rust_probestack` which wasmer 4.3.7 still references.)

## The procedure (production shape)

1. **Pre-window**: run the Phase-A ceremony for the NEW set (all continuing
   seats re-generate share requests; new seat keygens Ed25519 + MAYO2 on
   Unix). Coordinator assembles, each operator finalizes `keys.json`.
2. **Halt**: stop all current-set nodes. The halt is NOT height-atomic —
   seats will land within a few heights of each other; that is expected.
3. **Pick the anchor**: read each seat's executed tip (`executed tip payload
   injected` at startup, or the archive `blocks` table). The anchor is the
   HIGHEST (height, digest) observed across continuing seats. Every seat
   sets it in node.toml:
   ```toml
   [restart_anchor]
   height = <max executed height>
   digest = "<hex payload digest at that height>"
   ```
4. **Snapshot/backup**: copy `data_dir` per seat before touching anything.
5. **Swap**: install new `keys.json` + `node.toml` on every continuing and
   joining seat.
6. **Wipe consensus journal**: delete `{data_dir}/consensus` on EVERY seat
   (old + new). Keep `{data_dir}/app` untouched. New seats start empty.
7. **Restart** the new set together. View 1 anchors on the restart parent;
   seats behind the anchor fetch + execute the gap and then vote; a fresh
   joining seat backfills from peers (devnet: ~5k blocks in under a minute).
8. **Remove `restart_anchor`** from every seat's config once the new set is
   finalizing steadily — it is ignored once a seat passes `anchor.height`,
   but leaving it invites confusion in a later recovery.
9. **Verify** (all must pass before declaring the window done):
   - every new-set node advances height past the pre-halt tip;
   - the JOINING seat appears in proposer rotation and its index signs;
   - finalized certs carry the NEW threshold key (cert_len changes with n);
   - the RETIRED seat cannot participate: its key is absent from the new
     tracked set — peers refuse to even dial it;
   - no `replaying notarize`, `BadBlockHeight`, or `parent not executed`
     errors after warm-up;
   - app-state continuity: post-restart `app_hash` lineage is unbroken.

## W0 result — baseline formation

- 4 seats formed by full Phase-A ceremony (keygen-share → assemble →
  finalize), hybrid BLS12-381 + MAYO2, 3-of-4 quorum.
- Finalized steadily, ~9 blk/s devnet cadence, 616-byte certs. **PASS.**

## W1 result — add fifth seat — **PASS**

- Fresh n=5 deal; seat at .14 joined; index reshuffle handled via staged
  `ip-<octet>` mapping (validator_index = sorted Ed25519 position).
- Journal-replay panic reproduced and diagnosed (above); journal wipe +
  tip-aware `genesis()` + `restart_anchor` fix applied.
- Halt was non-atomic: seats landed at 4928/4929. Anchor set to (4929,
  `29ee90fa…`). All four continuing seats resumed finality at 4930+.
- The lagging seat (tip 4928) fetched + executed the gap inside `verify()`
  and began voting without manual repair.
- **The fresh seat (.14, height 0) backfilled all ~4,929 executed blocks
  from peers and entered leader rotation** — it was observed proposing
  (built payload at height 5598) and finalizing at tip.
- Certs grew 616 B → 802 B (n=4 → n=5 hybrid envelope). New threshold key
  in effect. Zero panics / BadBlockHeight / signer-replay / divergence.

## W2 result — remove fifth seat — **PASS**

- Retired seat .13; fresh n=4 deal over .10/.11/.12/.14. New sorted order:
  .12→0, .14→1, .10→2, .11→3.
- Halt landed all five at 6061 (`49b3523d…`) — anchor set accordingly.
- The n=4 set resumed finality immediately: certs back to 616 B, `peers=3`,
  all seats advancing past the anchor height. `app_hash` identical across
  seats — continuity unbroken.
- **Retired-seat negative test**: restarting node-13 with its stale w1
  keys leaves it isolated — its Ed25519 is not in the new tracked set, so
  peers refuse to dial it. It cannot propose, vote, or disrupt the new set.

## Honest caveats

- This is a **coordinated restart**, not runtime rotation. Do not represent
  it as in-protocol set change anywhere.
- The coordinator sees all BLS shares transiently during dealing (trusted
  setup). Phase C interactive DKG removes this.
- A joining seat with empty app state syncs via the resolver — expect a
  catch-up period before it contributes signatures.

## Operational addendum — live-chain apply variant (R4, 2026-11-15)

The R4 reshare ran as a **rolling apply on a live chain** (no coordinated
halt — the old set was already degraded and seats applied one-by-one over
~13h). It exposed two failure modes the devnet rehearsal never hit:

### Failure 1 — the tip kept moving during apply

With ≥quorum old-set seats still running, finality continued while the
fleet applied. The anchor picked at window-open was ~1,000 blocks stale
by the time the last seat restarted. Seats *above* `anchor.height`
silently self-anchor on their **local executed tip** (the `h > anchor`
fall-through in `genesis()`), while seats *below* it anchor on the
coordinator digest — a stale-low anchor therefore **forks the set**.

**Freeze variant (when a full halt isn't possible):**

1. Message every *continuing* seat to **stop** (not restart) — the
   finalized tip freezes. Apply seats may already be stopped; the
   continuing set must not produce blocks during the window.
2. Only then pick the anchor (below). Drift = 0 no matter how long the
   human apply takes.
3. After all new-set seats are anchored, restart the continuing set —
   new-set quorum forms and finality resumes.

### Failure 2 — anchor picked from cached telemetry

`jc-status`/explorer showed tip=6950159 frozen for ~10h while the chain
actually finalized to 6951156 — old-set quorum held at exactly 8 after
canary restarts. The "stall" was a telemetry artifact.

**Anchor verification (mandatory, ~2 min):**

- Pull the tip from **≥2 independent seats' live gRPC**, not status
  pages: `jc-explorer --grpc <peer>:9090 --listen 127.0.0.1:<port>`,
  then `GET /api/blocks?limit=1` — require identical height AND digest.
- Check freshness fields (`height_age_s`, `tip_age_s`) before trusting
  any "stall" reading: fresh probes + frozen tip = real stall;
  stale probes = broken telemetry, keep digging.
- Cross-check each seat's **executed** tip (`executed tip payload
  injected` at startup, or the archive `blocks` table) — the anchor is
  the HIGHEST executed (height, digest) across *continuing* seats, and
  every seat must be able to reach it (seats below catch up via
  `ensure_executed`; that fetch path is the long pole — verify it works
  before declaring quorum).

### Checklist delta vs. the base procedure

- [ ] Continuing set **stopped** (tip frozen) before anchor selection
- [ ] Tip verified via live gRPC on ≥2 seats, digest parity confirmed
- [ ] Each seat's executed-tip line collected; max (height, digest) = anchor
- [ ] Lagging seats' catch-up path smoke-tested (one seat first)
- [ ] `restart_anchor` removed fleet-wide after stable finality (step 8)
