//! generate-testnet-keys: Offline BLS12-381 DKG key generation for a static 3-node testnet.
//!
//! This tool generates key material for a testnet with a configurable number of validators.
//! It uses `commonware-cryptography::bls12381::dkg::deal_anonymous` — a trusted dealing
//! function suitable for bootstrapping a testnet (not a production DKG ceremony).
//!
//! For each validator, the output JSON contains:
//! - `validator_index`: The validator's index in the participant set (0-based)
//! - `share_index`: The 1-based share index in the BLS polynomial
//! - `bls_private_hex`: Hex-encoded BLS secret share private scalar
//! - `bls_public_hex`: Hex-encoded BLS public share for this participant
//! - `threshold_public_key_hex`: Hex-encoded group threshold public key (same for all validators)
//! - `threshold_required`: Number of shares required to produce a threshold signature
//! - `threshold_total`: Total number of shares distributed
//! - `ed25519_private_hex`: Hex-encoded Ed25519 identity private key for P2P authentication
//! - `ed25519_public_hex`: Hex-encoded Ed25519 identity public key
//! - `validator_public_keys`: Ordered list of all validators' Ed25519 public keys (hex)
//!
//! Usage:
//!   cargo run -- --output-dir ./testnet-keys --validators 3
//!
//! Output files:
//!   ./testnet-keys/validator-0/keys.json
//!   ./testnet-keys/validator-1/keys.json
//!   ./testnet-keys/validator-2/keys.json
//!
//! NOTE: This uses trusted dealing — the tool knows all shares at generation time.
//! This is acceptable for testnet bootstrapping. For production, use an interactive
//! DKG ceremony with `commonware-cryptography::bls12381::dkg::{Dealer, Player}`.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use commonware_codec::codec::{Encode, Read};
use commonware_cryptography::{
    bls12381::{
        dkg::{
            deal_anonymous, observe, Dealer, DealerLog, DealerPrivMsg, DealerPubMsg, Info, Output,
            Player, PlayerAck, SignedDealerLog,
        },
        primitives::{
            group::Share,
            sharing::{Mode, ModeVersion, Sharing},
            variant::MinSig,
        },
    },
    ed25519, Signer, Verifier,
};
use commonware_math::algebra::Random;
use commonware_parallel::Sequential;
use commonware_utils::{ordered::{Quorum, Set}, N3f1};
use rand::rngs::OsRng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

/// Key material for a single validator node.
#[derive(Serialize, Deserialize, Debug)]
pub struct ValidatorKeys {
    /// 0-based validator index in the static participant set.
    pub validator_index: usize,

    /// 1-based share index in the BLS polynomial (from commonware Participant.get()).
    pub share_index: u32,

    /// Hex-encoded BLS secret share private scalar.
    pub bls_private_hex: String,

    /// Hex-encoded BLS public share for this participant (G2 point for MinSig).
    pub bls_public_hex: String,

    /// Hex-encoded group threshold public key (same for all validators).
    /// This is the key used to verify threshold signatures without knowing individual shares.
    pub threshold_public_key_hex: String,

    /// Minimum number of shares required to produce a valid threshold signature.
    pub threshold_required: u32,

    /// Total number of validators/shares in the set.
    pub threshold_total: u32,

    /// Hex-encoded Ed25519 identity private key for P2P peer authentication.
    pub ed25519_private_hex: String,

    /// Hex-encoded Ed25519 identity public key.
    pub ed25519_public_hex: String,

    /// Ordered list of all validators' Ed25519 public keys (hex-encoded).
    /// Position in this list is the validator's 0-based index.
    pub validator_public_keys: Vec<String>,

    /// Serialized BLS `Sharing` (mode + total + public polynomial).
    /// Populated by `finalize` after a Phase A ceremony; absent in legacy
    /// devnet key files (the node re-deals deterministically instead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing_hex: Option<String>,

    /// Serialized BLS `Share` (participant index + private scalar).
    /// Populated by `finalize`; absent in legacy devnet key files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_hex: Option<String>,

    /// Hybrid validator identity (docs/PQ_PROTOCOL_AUTH.md §3) — only
    /// populated on Unix builds where sriracha-mayo keygen is available.
    /// `mayo_variant` is always "mayo2" (fixed-size signatures).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mayo_variant: Option<String>,

    /// Hex-encoded 24-byte MAYO2 secret seed (`SecretKey::from_seed` input).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mayo_private_hex: Option<String>,

    /// Hex-encoded MAYO2 compact public key (4912 bytes) for this validator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mayo_public_hex: Option<String>,

    /// MAYO2 public keys of ALL validators, in validator_index order — the
    /// same convention as `validator_public_keys`. The node sorts this table
    /// alongside the Ed25519 set at load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validator_mayo_public_keys: Option<Vec<String>>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Phase A ceremony subcommands. No subcommand → legacy devnet keygen.
    if let Some(cmd) = args.get(1).map(|s| s.as_str()) {
        match cmd {
            "keygen-share" => return cmd_keygen_share(&args[2..]),
            "assemble-genesis" => return cmd_assemble_genesis(&args[2..]),
            "finalize" => return cmd_finalize(&args[2..]),
            // Dealer-free DKG ceremony (docs/DKG-DESIGN.md) — Joint-Feldman
            // via commonware_cryptography::bls12381::dkg.
            "dkg-info" => return cmd_dkg_info(&args[2..]),
            "dkg-deal" => return cmd_dkg_deal(&args[2..]),
            "dkg-ack" => return cmd_dkg_ack(&args[2..]),
            "dkg-log" => return cmd_dkg_log(&args[2..]),
            "dkg-finalize" => return cmd_dkg_finalize(&args[2..]),
            "dkg-observe" => return cmd_dkg_observe(&args[2..]),
            "dkg-synthesize-genesis-output" => {
                return cmd_dkg_synthesize_genesis_output(&args[2..])
            }
            "seal" => return cmd_seal(&args[2..]),
            "unseal" => return cmd_unseal(&args[2..]),
            _ => {}
        }
    }

    legacy_main(&args);
}

/// Legacy centralized devnet keygen (trusted dealing, seeded RNG).
fn legacy_main(args: &[String]) {
    // Parse --output-dir and --validators flags
    let mut output_dir = "./testnet-keys".to_string();
    let mut num_validators: u32 = 3;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--output-dir" => {
                i += 1;
                if i < args.len() {
                    output_dir = args[i].clone();
                }
            }
            "--validators" => {
                i += 1;
                if i < args.len() {
                    num_validators = args[i]
                        .parse()
                        .expect("--validators must be a positive integer");
                }
            }
            "--help" | "-h" => {
                println!("Usage: generate-testnet-keys [--output-dir <dir>] [--validators <n>]");
                println!("  --output-dir  Output directory (default: ./testnet-keys)");
                println!("  --validators  Number of validators (default: 3)");
                return;
            }
            _ => {}
        }
        i += 1;
    }

    if num_validators < 1 {
        eprintln!("Error: --validators must be >= 1");
        std::process::exit(1);
    }

    println!(
        "Generating BLS12-381 DKG key material for {num_validators}-node testnet..."
    );
    println!("Output directory: {output_dir}");

    // Use a deterministic RNG seeded with zero for reproducible testnet keys.
    // IMPORTANT: For any production use, use OsRng instead of a seeded RNG.
    // This tool is intentionally deterministic for testnet bootstrapping.
    let mut rng = ChaCha8Rng::seed_from_u64(0);

    // Generate Ed25519 identity keys for all validators.
    let mut ed25519_keys: Vec<ed25519::PrivateKey> = Vec::new();
    for _ in 0..num_validators {
        ed25519_keys.push(ed25519::PrivateKey::random(&mut rng));
    }

    // Collect ordered list of all Ed25519 public keys (hex-encoded).
    let validator_public_keys: Vec<String> = ed25519_keys
        .iter()
        .map(|k| {
            use commonware_cryptography::Signer;
            hex::encode(k.public_key().encode().as_ref())
        })
        .collect();

    // Run offline BLS12-381 DKG using the trusted dealing function.
    //
    // `deal_anonymous` generates a random secret and distributes shares without
    // linking to specific participant identities. This is suitable for testnet
    // bootstrapping where we don't need to run the interactive P2P protocol.
    //
    // MinSig variant: public key in G2, signature in G1.
    // N3f1: BFT fault model requiring n >= 3f+1 participants.
    let n = NonZeroU32::new(num_validators).expect("num_validators > 0");
    let (sharing, shares) = deal_anonymous::<MinSig, N3f1>(&mut rng, Mode::NonZeroCounter, n);

    // Get the threshold public key (same for all validators).
    // For MinSig, the public key is a G2 point.
    let threshold_public_key = sharing.public();
    let threshold_public_key_hex = hex::encode(threshold_public_key.encode().as_ref());

    let threshold_required = sharing.required::<N3f1>();
    let threshold_total = sharing.total().get();

    // Hybrid validator identity (PQ_PROTOCOL_AUTH.md §3): per-validator
    // MAYO2 seed + compact public key. sriracha-mayo's signer is C-backed and
    // Unix-only, so on other platforms the MAYO fields are omitted — a node
    // running with `hybrid_consensus = true` will refuse to boot without them.
    #[cfg(unix)]
    let mayo_pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..num_validators)
        .map(|_| {
            use rand::RngCore;
            let mut seed = vec![0u8; 24]; // MAYO SK_SEED_BYTES
            rng.fill_bytes(&mut seed);
            let (pk, _sk) =
                sriracha_mayo::SecretKey::<sriracha_mayo::Mayo2>::from_seed(&seed)
                    .expect("MAYO2 keygen from 24-byte seed");
            let pk_bytes: &[u8] = pk.as_ref();
            (seed, pk_bytes.to_vec())
        })
        .collect();
    #[cfg(not(unix))]
    let mayo_pairs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

    let validator_mayo_public_keys: Option<Vec<String>> = if mayo_pairs.is_empty() {
        None
    } else {
        Some(
            mayo_pairs
                .iter()
                .map(|(_, pk)| hex::encode(pk))
                .collect(),
        )
    };

    println!(
        "BLS DKG complete: {threshold_required}/{threshold_total} threshold (N3f1)"
    );
    println!(
        "Group threshold public key: {}...",
        &threshold_public_key_hex[..std::cmp::min(32, threshold_public_key_hex.len())]
    );

    // Write key files for each validator.
    for (validator_index, (share, ed25519_key)) in
        shares.iter().zip(ed25519_keys.iter()).enumerate()
    {
        let dir = format!("{output_dir}/validator-{validator_index}");
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("Failed to create directory {dir}: {e}"));

        // Encode the BLS share private scalar using Encode trait.
        // Private.expose() allows us to access the inner scalar for encoding.
        let bls_private_hex = share.private.expose(|scalar| {
            hex::encode(scalar.encode().as_ref())
        });

        // Compute the BLS public share for this participant.
        // For MinSig, this is a G2 point.
        let bls_public_key = share.public::<MinSig>();
        let bls_public_hex = hex::encode(bls_public_key.encode().as_ref());

        // Encode the Ed25519 identity key pair.
        use commonware_cryptography::Signer;
        let ed25519_private_hex = hex::encode(ed25519_key.encode().as_ref());
        let ed25519_public = ed25519_key.public_key();
        let ed25519_public_hex = hex::encode(ed25519_public.encode().as_ref());

        let keys = ValidatorKeys {
            validator_index,
            share_index: share.index.get(),
            bls_private_hex,
            bls_public_hex,
            threshold_public_key_hex: threshold_public_key_hex.clone(),
            threshold_required,
            threshold_total,
            ed25519_private_hex,
            ed25519_public_hex,
            validator_public_keys: validator_public_keys.clone(),
            sharing_hex: None,
            share_hex: None,
            mayo_variant: mayo_pairs.get(validator_index).map(|_| "mayo2".to_string()),
            mayo_private_hex: mayo_pairs
                .get(validator_index)
                .map(|(seed, _)| hex::encode(seed)),
            mayo_public_hex: mayo_pairs
                .get(validator_index)
                .map(|(_, pk)| hex::encode(pk)),
            validator_mayo_public_keys: validator_mayo_public_keys.clone(),
        };

        let json = serde_json::to_string_pretty(&keys)
            .expect("JSON serialization of ValidatorKeys cannot fail");

        let path = format!("{dir}/keys.json");
        std::fs::write(&path, &json)
            .unwrap_or_else(|e| panic!("Failed to write {path}: {e}"));

        println!(
            "  validator-{validator_index}: {path} (share_index={}, ed25519_pub={}...)",
            share.index.get(),
            &keys.ed25519_public_hex[..std::cmp::min(16, keys.ed25519_public_hex.len())]
        );
    }

    println!("\nKey generation complete.");
    println!("Each validator-N/keys.json file contains:");
    println!("  - BLS secret share (bls_private_hex)");
    println!("  - BLS public share (bls_public_hex)");
    println!("  - Group threshold public key (threshold_public_key_hex)");
    println!("  - Ed25519 identity key pair (ed25519_private_hex, ed25519_public_hex)");
    println!("  - Ordered validator set (validator_public_keys)");
    println!("\nSet bls_key_path in your node config to point to the appropriate keys.json.");
    #[cfg(unix)]
    println!("MAYO2 identity material included — usable with hybrid_consensus = true.");
    #[cfg(not(unix))]
    println!("NOTE: MAYO keygen is Unix-only (sriracha-mayo). keys.json lacks hybrid");
    #[cfg(not(unix))]
    println!("      identity fields — regenerate on Unix/WSL to use hybrid_consensus.");
    println!("WARNING: These keys were generated with a seeded RNG for reproducibility.");
    println!("         Do NOT use this tool for production deployments — use OsRng.");
}

// ──────────────────────────────────────────────
// Phase A ceremony tooling
// ──────────────────────────────────────────────
//
// Flow (see drafts/VALIDATOR_SET_DKG_PLAN.md §5):
//   1. Each validator runs `keygen-share` locally → publishes share-request.json
//      (Ed25519 pubkey + P2P address + self-attestation). Private key never leaves.
//   2. Coordinator collects all share-requests → runs `assemble-genesis` →
//      deals BLS shares with OsRng, emits per-validator bls-share.json
//      (distributed privately), shared.json, and node-<i>.toml templates.
//   3. Each validator runs `finalize` to merge their bls-share.json into
//      keys.json. The node loads sharing_hex/share_hex directly.
//
// Trust model: the coordinator transiently sees all BLS shares (trusted
// dealing). Ed25519 identity keys are self-generated and never shared.
// Phase C replaces dealing with the interactive Dealer/Player protocol.

/// Namespace for the Ed25519 attestation binding a share-request to its key.
const DKG_ATTEST_NAMESPACE: &[u8] = b"junoclaw-dkg-v1";

/// Public share-request published by each validator (output of keygen-share).
#[derive(Serialize, Deserialize, Debug)]
pub struct ShareRequest {
    /// Human-readable validator moniker (e.g. "ffern-1").
    pub moniker: String,
    /// P2P listen address peers will use to reach this validator (host:port).
    pub p2p_address: String,
    /// Hex-encoded Ed25519 identity public key.
    pub ed25519_public_hex: String,
    /// Hex-encoded MAYO2 compact public key (4912 B) — hybrid validator
    /// identity (PQ_PROTOCOL_AUTH.md §3). Present when the operator ran
    /// keygen-share on a Unix host with sriracha-mayo. All-or-none: the
    /// coordinator rejects a mixed set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mayo_public_hex: Option<String>,
    /// Hex-encoded Ed25519 signature binding this identity — over
    /// `ed25519_pk` alone (classical) or `ed25519_pk || mayo_pk` when
    /// `mayo_public_hex` is present (namespace "junoclaw-dkg-v1").
    /// Proves possession of the private key(s).
    pub attestation_hex: String,
}

/// Private BLS share package sent by the coordinator to one validator.
#[derive(Serialize, Deserialize, Debug)]
pub struct BlsSharePackage {
    /// 0-based validator index (sorted Ed25519 position).
    pub validator_index: usize,
    /// 1-based BLS share index.
    pub share_index: u32,
    /// Ed25519 pubkey this share is bound to (must match keys.json).
    pub ed25519_public_hex: String,
    /// Serialized `Sharing` (public polynomial — same for all validators).
    pub sharing_hex: String,
    /// Serialized `Share` (this validator's private share).
    pub share_hex: String,
    /// Hex-encoded group threshold public key.
    pub threshold_public_key_hex: String,
    /// Minimum shares required for a threshold signature.
    pub threshold_required: u32,
    /// Total shares in the set.
    pub threshold_total: u32,
    /// Ordered validator set (sorted Ed25519 pubkeys, hex).
    pub validator_public_keys: Vec<String>,
    /// MAYO2 compact pubkeys in the same sorted order as
    /// `validator_public_keys` — the hybrid identity table the node loads
    /// under `hybrid_consensus`. None when the ceremony ran classical-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validator_mayo_public_keys: Option<Vec<String>>,
}

/// Public ceremony output — the assembled validator set (no secrets).
#[derive(Serialize, Deserialize, Debug)]
pub struct SharedCeremonyOutput {
    pub chain_id: String,
    pub threshold_public_key_hex: String,
    pub threshold_required: u32,
    pub threshold_total: u32,
    /// Ordered by sorted Ed25519 pubkey — position = validator_index.
    pub validators: Vec<SharedValidator>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SharedValidator {
    pub validator_index: usize,
    pub moniker: String,
    pub p2p_address: String,
    pub ed25519_public_hex: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mayo_public_hex: Option<String>,
}

fn get_arg<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// `keygen-share` — run by EACH validator on their own machine.
///
/// Generates the Ed25519 identity keypair with OsRng, writes a partial
/// keys.json (BLS fields empty until `finalize`), and a share-request.json
/// to publish to the coordinator.
fn cmd_keygen_share(args: &[String]) {
    let output_dir = get_arg(args, "--output-dir").unwrap_or("./my-validator").to_string();
    let moniker = get_arg(args, "--name").unwrap_or("validator").to_string();
    let p2p_address = get_arg(args, "--p2p").unwrap_or("0.0.0.0:7001").to_string();

    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys keygen-share [--output-dir <dir>] [--name <moniker>] [--p2p <host:port>]");
        return;
    }

    let mut rng = OsRng;
    let ed_key = ed25519::PrivateKey::random(&mut rng);
    let ed_pub = ed_key.public_key();
    let ed_pub_bytes = ed_pub.encode();
    let ed25519_public_hex = hex::encode(ed_pub_bytes.as_ref());
    let ed25519_private_hex = hex::encode(ed_key.encode().as_ref());

    // Hybrid validator identity (PQ_PROTOCOL_AUTH.md §3): MAYO2 seed +
    // compact pk with real entropy (OsRng). sriracha-mayo keygen is
    // Unix-only; elsewhere the fields stay None and the share-request
    // carries no MAYO material (coordinator enforces all-or-none).
    #[cfg(unix)]
    let mayo: Option<(Vec<u8>, Vec<u8>)> = {
        use rand::RngCore;
        let mut seed = vec![0u8; 24]; // MAYO SK_SEED_BYTES
        OsRng.fill_bytes(&mut seed);
        let (pk, _sk) = sriracha_mayo::SecretKey::<sriracha_mayo::Mayo2>::from_seed(&seed)
            .expect("MAYO2 keygen from 24-byte seed");
        let pk_bytes: &[u8] = pk.as_ref();
        Some((seed, pk_bytes.to_vec()))
    };
    #[cfg(not(unix))]
    let mayo: Option<(Vec<u8>, Vec<u8>)> = None;
    let (mayo_private_hex, mayo_public_hex) = match &mayo {
        Some((seed, pk)) => (Some(hex::encode(seed)), Some(hex::encode(pk))),
        None => (None, None),
    };

    // Attestation: sign `ed25519_pk` (classical) or `ed25519_pk || mayo_pk`
    // (hybrid) — proves possession and binds both halves of the identity.
    let mut attest_msg = ed_pub_bytes.to_vec();
    if let Some((_, mayo_pk)) = &mayo {
        attest_msg.extend_from_slice(mayo_pk);
    }
    let attestation = ed_key.sign(DKG_ATTEST_NAMESPACE, &attest_msg);
    let attestation_hex = hex::encode(attestation.encode().as_ref());

    std::fs::create_dir_all(&output_dir)
        .unwrap_or_else(|e| panic!("Failed to create {output_dir}: {e}"));

    // Partial keys.json — BLS fields filled by `finalize` after the ceremony.
    let keys = ValidatorKeys {
        validator_index: 0,
        share_index: 0,
        bls_private_hex: String::new(),
        bls_public_hex: String::new(),
        threshold_public_key_hex: String::new(),
        threshold_required: 0,
        threshold_total: 0,
        ed25519_private_hex,
        ed25519_public_hex: ed25519_public_hex.clone(),
        validator_public_keys: Vec::new(),
        sharing_hex: None,
        share_hex: None,
        mayo_variant: mayo.as_ref().map(|_| "mayo2".to_string()),
        mayo_private_hex,
        mayo_public_hex: mayo_public_hex.clone(),
        validator_mayo_public_keys: None,
    };
    let keys_path = format!("{output_dir}/keys.json");
    std::fs::write(&keys_path, serde_json::to_string_pretty(&keys).unwrap())
        .unwrap_or_else(|e| panic!("Failed to write {keys_path}: {e}"));

    let request = ShareRequest {
        moniker: moniker.clone(),
        p2p_address: p2p_address.clone(),
        ed25519_public_hex: ed25519_public_hex.clone(),
        mayo_public_hex,
        attestation_hex,
    };
    let req_path = format!("{output_dir}/share-request.json");
    std::fs::write(&req_path, serde_json::to_string_pretty(&request).unwrap())
        .unwrap_or_else(|e| panic!("Failed to write {req_path}: {e}"));

    println!("Validator identity generated (OsRng).");
    println!("  moniker:   {moniker}");
    println!("  ed25519:   {}...", &ed25519_public_hex[..16]);
    match mayo {
        Some(_) => println!("  mayo2:     included in share-request (hybrid identity)"),
        None => println!(
            "  mayo2:     NOT generated (Unix-only) — re-run on Linux/macOS for hybrid consensus"
        ),
    }
    println!("  keys:      {keys_path}  (PRIVATE — never share)");
    println!("  request:   {req_path}  (publish to coordinator)");
    println!("\nNext: send share-request.json to the coordinator.");
    println!("After the ceremony, run `finalize` with the bls-share.json you receive.");
}

/// `assemble-genesis` — run by the COORDINATOR with all share-requests.
///
/// Verifies attestations, sorts validators by Ed25519 pubkey (deterministic
/// index assignment matching the node's sorted-position logic), deals BLS
/// shares with OsRng, and emits per-validator packages + shared.json +
/// node-<i>.toml templates.
fn cmd_assemble_genesis(args: &[String]) {
    let input_dir = get_arg(args, "--input-dir").unwrap_or("./share-requests").to_string();
    let output_dir = get_arg(args, "--output-dir").unwrap_or("./ceremony-out").to_string();
    let chain_id = get_arg(args, "--chain-id").unwrap_or("junoclaw-1").to_string();
    if chain_id == "junoclaw-1" {
        println!(
            "WARNING: --chain-id not set, defaulting to \"junoclaw-1\". \
             Use a dedicated id like \"junoclaw-g1\" for testnets — \"junoclaw-1\" reads like a mainnet id."
        );
    }

    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys assemble-genesis [--input-dir <dir>] [--output-dir <dir>] [--chain-id <id>]");
        return;
    }

    // Load every *.json in input-dir as a share-request.
    let mut requests: Vec<ShareRequest> = Vec::new();
    let entries = std::fs::read_dir(&input_dir)
        .unwrap_or_else(|e| panic!("Failed to read {input_dir}: {e}"));
    for entry in entries {
        let path = entry.expect("read_dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
        let req: ShareRequest = serde_json::from_str(&contents)
            .unwrap_or_else(|e| panic!("{} is not a valid share-request: {e}", path.display()));
        requests.push(req);
    }

    if requests.len() < 4 {
        eprintln!("Error: need at least 4 share-requests for N3f1 consensus (got {})", requests.len());
        std::process::exit(1);
    }

    // Verify each attestation: signature over the pubkey under DKG namespace.
    for req in &requests {
        let pk_bytes = hex::decode(&req.ed25519_public_hex)
            .unwrap_or_else(|_| panic!("{}: bad ed25519_public_hex", req.moniker));
        let sig_bytes = hex::decode(&req.attestation_hex)
            .unwrap_or_else(|_| panic!("{}: bad attestation_hex", req.moniker));
        let pk = ed25519::PublicKey::read_cfg(&mut &pk_bytes[..], &())
            .unwrap_or_else(|_| panic!("{}: undecodable ed25519 pubkey", req.moniker));
        let sig = ed25519::Signature::read_cfg(&mut &sig_bytes[..], &())
            .unwrap_or_else(|_| panic!("{}: undecodable attestation", req.moniker));
        // Hybrid requests attest over `ed25519_pk || mayo_pk`; classical
        // requests over `ed25519_pk` alone — same rule the operator used.
        let mut attest_msg = pk_bytes.clone();
        if let Some(mh) = &req.mayo_public_hex {
            let mpk = hex::decode(mh)
                .unwrap_or_else(|_| panic!("{}: bad mayo_public_hex", req.moniker));
            if mpk.len() != 4912 {
                panic!("{}: mayo_public_hex must be a MAYO2 compact pk (4912 bytes)", req.moniker);
            }
            attest_msg.extend_from_slice(&mpk);
        }
        if !pk.verify(DKG_ATTEST_NAMESPACE, &attest_msg, &sig) {
            panic!("{}: attestation verification failed — pubkey not signed by its owner", req.moniker);
        }
    }
    println!("Verified {} attestations.", requests.len());

    // MAYO identity is all-or-none: hybrid consensus needs every
    // validator's MAYO pk — a mixed set would produce an unusable table.
    let any_mayo = requests.iter().any(|r| r.mayo_public_hex.is_some());
    let all_mayo = requests.iter().all(|r| r.mayo_public_hex.is_some());
    if any_mayo && !all_mayo {
        let missing: Vec<&str> = requests
            .iter()
            .filter(|r| r.mayo_public_hex.is_none())
            .map(|r| r.moniker.as_str())
            .collect();
        panic!(
            "mixed ceremony: some share-requests carry MAYO identity but these don't: {missing:?} — \
             either all validators provide mayo_public_hex or none may"
        );
    }

    // Sort by Ed25519 pubkey bytes — deterministic validator_index, and it
    // matches the node's sorted-position share selection.
    requests.sort_by(|a, b| {
        let ab = hex::decode(&a.ed25519_public_hex).unwrap();
        let bb = hex::decode(&b.ed25519_public_hex).unwrap();
        ab.cmp(&bb)
    });

    // Duplicate pubkey check.
    for w in requests.windows(2) {
        if w[0].ed25519_public_hex == w[1].ed25519_public_hex {
            panic!("duplicate ed25519 pubkey: {} and {}", w[0].moniker, w[1].moniker);
        }
    }

    let validator_public_keys: Vec<String> =
        requests.iter().map(|r| r.ed25519_public_hex.clone()).collect();
    let validator_mayo_public_keys: Option<Vec<String>> = if all_mayo {
        Some(
            requests
                .iter()
                .map(|r| r.mayo_public_hex.clone().unwrap())
                .collect(),
        )
    } else {
        println!("Classical-only ceremony (no MAYO identity) — hybrid_consensus will refuse to boot.");
        None
    };

    // Deal BLS shares with OsRng — real entropy, not the seeded devnet path.
    let n = NonZeroU32::new(requests.len() as u32).unwrap();
    let mut rng = OsRng;
    let (sharing, shares) = deal_anonymous::<MinSig, N3f1>(&mut rng, Mode::NonZeroCounter, n);

    let threshold_public_key_hex = hex::encode(sharing.public().encode().as_ref());
    let threshold_required = sharing.required::<N3f1>();
    let threshold_total = sharing.total().get();
    let sharing_hex = hex::encode(sharing.encode().as_ref());

    println!("BLS dealing complete: {threshold_required}/{threshold_total} threshold (N3f1, OsRng)");
    println!("Group threshold pubkey: {}...", &threshold_public_key_hex[..32]);

    std::fs::create_dir_all(&output_dir)
        .unwrap_or_else(|e| panic!("Failed to create {output_dir}: {e}"));

    // Per-validator private share package + node.toml template.
    for (i, (req, share)) in requests.iter().zip(shares.iter()).enumerate() {
        let dir = format!("{output_dir}/validator-{i}");
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("Failed to create {dir}: {e}"));

        let package = BlsSharePackage {
            validator_index: i,
            share_index: share.index.get(),
            ed25519_public_hex: req.ed25519_public_hex.clone(),
            sharing_hex: sharing_hex.clone(),
            share_hex: hex::encode(share.encode().as_ref()),
            threshold_public_key_hex: threshold_public_key_hex.clone(),
            threshold_required,
            threshold_total,
            validator_public_keys: validator_public_keys.clone(),
            validator_mayo_public_keys: validator_mayo_public_keys.clone(),
        };
        let pkg_path = format!("{dir}/bls-share.json");
        std::fs::write(&pkg_path, serde_json::to_string_pretty(&package).unwrap())
            .unwrap_or_else(|e| panic!("Failed to write {pkg_path}: {e}"));

        // node.toml template — peers = all OTHER validators.
        let mut toml = format!(
            "validator_index = {i}\n\
             chain_id = \"{chain_id}\"\n\
             p2p_listen = \"0.0.0.0:7001\"\n\
             grpc_listen = \"0.0.0.0:9090\"\n\
             bls_key_path = \"/keys/keys.json\"\n\
             identity_key_path = \"/keys/keys.json\"\n\
             data_dir = \"/data\"\n\
             genesis_path = \"/config/genesis.json\"\n\
             mempool_max_pending = 10000\n\
             leader_timeout_ms = 3000\n\
             certification_timeout_ms = 5000\n\
             pruning = \"validator\"\n\
             # insecure_devnet must stay unset on a real network\n"
        );
        if validator_mayo_public_keys.is_some() {
            toml.push_str("hybrid_consensus = true\n");
        }
        for (j, peer) in requests.iter().enumerate() {
            if j == i {
                continue;
            }
            toml.push_str(&format!(
                "\n[[peers]]\npublic_key = \"{}\"\naddress = \"{}\"\n",
                peer.ed25519_public_hex, peer.p2p_address
            ));
        }
        let toml_path = format!("{dir}/node-{i}.toml");
        std::fs::write(&toml_path, toml)
            .unwrap_or_else(|e| panic!("Failed to write {toml_path}: {e}"));

        println!(
            "  validator-{i}: {} ({}) — {pkg_path} [PRIVATE]",
            req.moniker, req.p2p_address
        );
    }

    // shared.json — public ceremony output.
    let shared = SharedCeremonyOutput {
        chain_id,
        threshold_public_key_hex,
        threshold_required,
        threshold_total,
        validators: requests
            .iter()
            .enumerate()
            .map(|(i, r)| SharedValidator {
                validator_index: i,
                moniker: r.moniker.clone(),
                p2p_address: r.p2p_address.clone(),
                ed25519_public_hex: r.ed25519_public_hex.clone(),
                mayo_public_hex: r.mayo_public_hex.clone(),
            })
            .collect(),
    };
    let shared_path = format!("{output_dir}/shared.json");
    std::fs::write(&shared_path, serde_json::to_string_pretty(&shared).unwrap())
        .unwrap_or_else(|e| panic!("Failed to write {shared_path}: {e}"));

    println!("\nCeremony assembly complete.");
    println!("  shared.json: {shared_path} (public)");
    println!("  validator-<i>/bls-share.json: send each to its owner over a secure channel");
    println!("  validator-<i>/node-<i>.toml: config template for each validator");
    println!("\nWARNING: bls-share.json files contain secret shares. Distribute");
    println!("         privately (encrypted DM, age, etc.) and delete local copies.");
}

/// `finalize` — run by EACH validator after receiving bls-share.json.
///
/// Merges the dealt share into their keys.json and verifies the share's
/// public key matches the sharing polynomial at its index.
fn cmd_finalize(args: &[String]) {
    let keys_path = get_arg(args, "--keys").unwrap_or("./keys.json").to_string();
    let share_path = get_arg(args, "--bls-share").unwrap_or("./bls-share.json").to_string();

    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys finalize [--keys <keys.json>] [--bls-share <bls-share.json>]");
        return;
    }

    let mut keys: ValidatorKeys = serde_json::from_str(
        &std::fs::read_to_string(&keys_path)
            .unwrap_or_else(|e| panic!("Failed to read {keys_path}: {e}")),
    )
    .unwrap_or_else(|e| panic!("{keys_path} is not valid keys.json: {e}"));

    let package: BlsSharePackage = serde_json::from_str(
        &std::fs::read_to_string(&share_path)
            .unwrap_or_else(|e| panic!("Failed to read {share_path}: {e}")),
    )
    .unwrap_or_else(|e| panic!("{share_path} is not valid bls-share.json: {e}"));

    // The package must be addressed to OUR ed25519 key.
    if package.ed25519_public_hex != keys.ed25519_public_hex {
        eprintln!("Error: bls-share.json is bound to a different Ed25519 key.");
        eprintln!("  ours:    {}", keys.ed25519_public_hex);
        eprintln!("  package: {}", package.ed25519_public_hex);
        std::process::exit(1);
    }

    // Deserialize sharing + share; verify the share belongs to the polynomial.
    let sharing_bytes = hex::decode(&package.sharing_hex).expect("bad sharing_hex");
    let share_bytes = hex::decode(&package.share_hex).expect("bad share_hex");
    let n = NonZeroU32::new(package.threshold_total).expect("threshold_total > 0");
    let sharing = Sharing::<MinSig>::read_cfg(&mut &sharing_bytes[..], &(n, ModeVersion::v0()))
        .expect("undecodable sharing");
    let share = Share::read_cfg(&mut &share_bytes[..], &()).expect("undecodable share");

    let expected_pub = sharing
        .partial_public(share.index)
        .expect("share index out of range for polynomial");
    let actual_pub = share.public::<MinSig>();
    if expected_pub.encode().as_ref() != actual_pub.encode().as_ref() {
        eprintln!("Error: share does not match the sharing polynomial — package corrupt or mismatched.");
        std::process::exit(1);
    }

    // Merge into keys.json.
    keys.validator_index = package.validator_index;
    keys.share_index = package.share_index;
    keys.bls_private_hex = share
        .private
        .expose(|scalar| hex::encode(scalar.encode().as_ref()));
    keys.bls_public_hex = hex::encode(actual_pub.encode().as_ref());
    keys.threshold_public_key_hex = package.threshold_public_key_hex;
    keys.threshold_required = package.threshold_required;
    keys.threshold_total = package.threshold_total;
    keys.validator_public_keys = package.validator_public_keys;
    keys.sharing_hex = Some(package.sharing_hex);
    keys.share_hex = Some(package.share_hex);

    // Hybrid identity: the ceremony's MAYO table (if any) must contain
    // the pk this operator generated at keygen-share time.
    match (
        &package.validator_mayo_public_keys,
        &keys.mayo_public_hex,
    ) {
        (Some(table), Some(own)) => {
            match table.get(package.validator_index) {
                Some(t) if t == own => {}
                _ => {
                    eprintln!("Error: our mayo_public_hex is absent from the ceremony table at index {}", package.validator_index);
                    std::process::exit(1);
                }
            }
            keys.validator_mayo_public_keys = Some(table.clone());
            keys.mayo_variant = Some("mayo2".to_string());
        }
        (Some(_), None) => {
            eprintln!("Error: ceremony is hybrid but keys.json lacks MAYO identity — re-run keygen-share on Unix.");
            std::process::exit(1);
        }
        (None, Some(_)) => {
            eprintln!("Warning: keys.json has MAYO identity but ceremony ran classical-only — mayo fields unused.");
        }
        (None, None) => {}
    }

    std::fs::write(&keys_path, serde_json::to_string_pretty(&keys).unwrap())
        .unwrap_or_else(|e| panic!("Failed to write {keys_path}: {e}"));

    println!("keys.json finalized.");
    println!("  validator_index: {}", keys.validator_index);
    println!("  share_index:     {}", keys.share_index);
    println!("  threshold:       {}/{}", keys.threshold_required, keys.threshold_total);
    println!("\nPoint bls_key_path + identity_key_path at {keys_path} in node.toml.");
    println!("Delete bls-share.json — its contents now live in keys.json.");
}

// ============================================================================
// Dealer-free DKG ceremony (docs/DKG-DESIGN.md)
//
// Joint-Feldman DKG over `commonware_cryptography::bls12381::dkg`. Every
// participant is both a Dealer and a Player; no party ever holds the group
// secret. Files move over the operators' existing secure channel:
//
//   coordinator: dkg-info      → publish dkg-info.json
//   each op:     dkg-deal      → broadcast dealing-pub-*, DM dealing-priv-*
//   each op:     dkg-ack       → broadcast ack-*
//   each dealer: dkg-log       → broadcast log-*
//   each op:     dkg-finalize  → produces bls-share.json (merge via finalize)
//   anyone:      dkg-observe   → produces shared.json from public logs only
//
// Dealer state is deterministic from (info, identity, dealer.seed): the seed
// file lets `dkg-log` rebuild the exact dealer that emitted the dealing.
// ============================================================================

/// DKG namespace binding: `junoclaw-dkg:<chain_id>` — transcripts are scoped
/// per chain so a dealing can never be replayed across networks.
fn dkg_namespace(chain_id: &str) -> Vec<u8> {
    format!("junoclaw-dkg:{chain_id}").into_bytes()
}

type DkgPk = ed25519::PublicKey;
type DkgInfo = Info<MinSig, DkgPk>;

/// Serializable form of a round `Info` — dealers == players for fresh deals.
#[derive(Serialize, Deserialize, Debug)]
struct DkgInfoFile {
    chain_id: String,
    round: u64,
    /// Sorted Ed25519 pubkeys (hex) — dealing participants.
    dealers: Vec<String>,
    /// Sorted Ed25519 pubkeys (hex) — share recipients.
    players: Vec<String>,
    /// Serialized `Output` of the previous round (resharing only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    previous_output_hex: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct DealingPrivFile {
    dealer: String,
    player: String,
    pub_msg_hex: String,
    /// Legacy plaintext payload — only present in files dealt with
    /// `--plaintext-priv` (pre-encryption format; still accepted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priv_msg_hex: Option<String>,
    /// Encrypted payload (D5): the share sealed to the recipient's
    /// Ed25519-derived X25519 key. The coordinator distributing this
    /// file cannot read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enc: Option<EncField>,
}

/// Encrypted `priv_msg` blob inside a `dealing-priv-*` file.
/// Scheme: ephemeral X25519 DH to the recipient's Ed25519-mapped
/// Montgomery key → SHA-256 domain-separated key → XChaCha20-Poly1305.
#[derive(Serialize, Deserialize, Debug)]
struct EncField {
    /// Always "x25519-xchacha20poly1305-v1" — lets the format evolve.
    scheme: String,
    /// Ephemeral X25519 public key (hex).
    ephem_hex: String,
    /// 24-byte XChaCha20 nonce (hex).
    nonce_hex: String,
    /// Ciphertext incl. Poly1305 tag (hex).
    ct_hex: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct DealingPubFile {
    dealer: String,
    pub_msg_hex: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct AckFile {
    player: String,
    dealer: String,
    ack_hex: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct LogFile {
    dealer: String,
    log_hex: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct DkgOutputFile {
    chain_id: String,
    round: u64,
    output_hex: String,
    threshold_public_key_hex: String,
    threshold_required: u32,
    threshold_total: u32,
}

fn decode_ed_pk(hex_str: &str) -> DkgPk {
    let bytes = hex::decode(hex_str).unwrap_or_else(|_| panic!("bad ed25519 hex: {hex_str}"));
    DkgPk::read_cfg(&mut &bytes[..], &())
        .unwrap_or_else(|_| panic!("undecodable ed25519 pubkey: {hex_str}"))
}

fn pk_hex(pk: &DkgPk) -> String {
    hex::encode(pk.encode().as_ref())
}

fn short(hex_str: &str) -> &str {
    &hex_str[..8.min(hex_str.len())]
}

// --- dealing-priv encryption (D5) -----------------------------------------
//
// Recipients are identified by Ed25519 keys, which can't encrypt. We use
// the standard libsodium mapping: X25519 scalar = clamp(SHA-512(seed)),
// and the public Montgomery u-coordinate = (1+y)/(1-y) of the Edwards
// point. Files carry an ephemeral-static X25519 + XChaCha20-Poly1305
// ciphertext, so a relaying coordinator holds only sealed shares.

fn x25519_scalar_from_ed_seed(seed: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let h = sha2::Sha512::digest(seed);
    let mut s = [0u8; 32];
    s.copy_from_slice(&h[..32]);
    s[0] &= 248;
    s[31] &= 127;
    s[31] |= 64;
    s
}

fn x25519_pub_from_ed_pub(ed: &[u8]) -> [u8; 32] {
    use curve25519_dalek_ng::edwards::CompressedEdwardsY;
    let point = CompressedEdwardsY(ed.try_into().expect("ed pubkey must be 32 bytes"))
        .decompress()
        .expect("recipient ed25519 pubkey is not on the curve");
    point.to_montgomery().0
}

fn dealing_key(shared: &[u8; 32]) -> [u8; 32] {
    use sha2::Digest;
    let mut k = [0u8; 32];
    k.copy_from_slice(&sha2::Sha256::new()
        .chain_update(b"junoclaw-dealing-v1")
        .chain_update(shared)
        .finalize());
    k
}

fn encrypt_for_recipient(recipient_ed_hex: &str, plaintext: &[u8]) -> EncField {
    use chacha20poly1305::aead::{Aead, KeyInit};
    let ed = hex::decode(recipient_ed_hex).expect("bad recipient ed25519 hex");
    let recip = x25519_dalek::PublicKey::from(x25519_pub_from_ed_pub(&ed));
    let eph = x25519_dalek::StaticSecret::random_from_rng(OsRng);
    let shared = eph.diffie_hellman(&recip);
    let cipher = chacha20poly1305::XChaCha20Poly1305::new_from_slice(&dealing_key(shared.as_bytes()))
        .expect("key length");
    let mut nonce = [0u8; 24];
    use rand::RngCore;
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(chacha20poly1305::XNonce::from_slice(&nonce), plaintext)
        .expect("aead encrypt");
    EncField {
        scheme: "x25519-xchacha20poly1305-v1".to_string(),
        ephem_hex: hex::encode(x25519_dalek::PublicKey::from(&eph).to_bytes()),
        nonce_hex: hex::encode(nonce),
        ct_hex: hex::encode(ct),
    }
}

fn decrypt_for_me(enc: &EncField, my_seed: &[u8]) -> Vec<u8> {
    use chacha20poly1305::aead::{Aead, KeyInit};
    assert_eq!(
        enc.scheme, "x25519-xchacha20poly1305-v1",
        "unknown dealing-priv scheme"
    );
    let me = x25519_dalek::StaticSecret::from(x25519_scalar_from_ed_seed(my_seed));
    let eph_bytes: [u8; 32] = hex::decode(&enc.ephem_hex)
        .expect("bad ephem hex")
        .try_into()
        .expect("ephem must be 32 bytes");
    let shared = me.diffie_hellman(&x25519_dalek::PublicKey::from(eph_bytes));
    let cipher = chacha20poly1305::XChaCha20Poly1305::new_from_slice(&dealing_key(shared.as_bytes()))
        .expect("key length");
    let nonce = hex::decode(&enc.nonce_hex).expect("bad nonce hex");
    cipher
        .decrypt(
            chacha20poly1305::XNonce::from_slice(&nonce),
            hex::decode(&enc.ct_hex).expect("bad ct hex").as_ref(),
        )
        .expect("dealing-priv decryption failed — is this file addressed to this keys.json?")
}

/// Return the plaintext `priv_msg` bytes whether the file is sealed (new)
/// or legacy plaintext. `my_seed` is the recipient's raw Ed25519 seed.
fn resolve_priv_bytes(f: &DealingPrivFile, my_seed: &[u8]) -> Vec<u8> {
    if let Some(enc) = &f.enc {
        return decrypt_for_me(enc, my_seed);
    }
    hex::decode(
        f.priv_msg_hex
            .as_deref()
            .expect("dealing-priv file has neither priv_msg_hex nor enc"),
    )
    .expect("bad priv_msg_hex")
}

/// Seal an arbitrary file to a recipient's Ed25519 public key (jc-share
/// phase 1). Emits the same x25519-xchacha20poly1305-v1 JSON envelope used
/// for dealing-priv files, so any transport sees ciphertext only.
fn cmd_seal(args: &[String]) {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys seal --to <ed25519 public hex> --in <file> --out <file>");
        return;
    }
    let to = get_arg(args, "--to").expect("--to <ed25519 public hex> required");
    let input = get_arg(args, "--in").expect("--in <file> required");
    let output = get_arg(args, "--out").expect("--out <file> required");
    let plaintext = std::fs::read(input).unwrap_or_else(|e| panic!("read {input}: {e}"));
    let enc = encrypt_for_recipient(to, &plaintext);
    write_json(output, &enc);
    println!(
        "sealed {} bytes -> {output} (recipient {})",
        plaintext.len(),
        short(to)
    );
}

/// Unseal a file addressed to our keys.json Ed25519 identity.
fn cmd_unseal(args: &[String]) {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys unseal [--keys <keys.json>] --in <file> --out <file>");
        return;
    }
    let keys_path = get_arg(args, "--keys").unwrap_or("./keys.json");
    let input = get_arg(args, "--in").expect("--in <file> required");
    let output = get_arg(args, "--out").expect("--out <file> required");
    let keys: ValidatorKeys = load_json(keys_path);
    let enc: EncField = load_json(input);
    let seed = hex::decode(&keys.ed25519_private_hex).expect("bad ed25519_private_hex");
    let plaintext = decrypt_for_me(&enc, &seed);
    std::fs::write(output, &plaintext).unwrap_or_else(|e| panic!("write {output}: {e}"));
    println!("unsealed {} bytes -> {output}", plaintext.len());
}

fn load_json<T: for<'de> Deserialize<'de>>(path: &str) -> T {
    serde_json::from_str(
        &std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("Failed to read {path}: {e}")),
    )
    .unwrap_or_else(|e| panic!("{path} is not valid: {e}"))
}

fn write_json<T: Serialize>(path: &str, value: &T) {
    std::fs::write(path, serde_json::to_string_pretty(value).unwrap())
        .unwrap_or_else(|e| panic!("Failed to write {path}: {e}"));
}

fn load_identity(keys_path: &str) -> (ValidatorKeys, ed25519::PrivateKey) {
    let keys: ValidatorKeys = load_json(keys_path);
    let sk_bytes =
        hex::decode(&keys.ed25519_private_hex).expect("keys.json: bad ed25519_private_hex");
    let sk = ed25519::PrivateKey::read_cfg(&mut &sk_bytes[..], &())
        .expect("keys.json: undecodable ed25519 private key");
    (keys, sk)
}

fn build_info(file: &DkgInfoFile) -> DkgInfo {
    let dealers: Vec<DkgPk> = file.dealers.iter().map(|h| decode_ed_pk(h)).collect();
    let players: Vec<DkgPk> = file.players.iter().map(|h| decode_ed_pk(h)).collect();
    let n = NonZeroU32::new(players.len() as u32).expect("players must be non-empty");
    let previous = file.previous_output_hex.as_ref().map(|hex_str| {
        let bytes = hex::decode(hex_str).expect("bad previous_output_hex");
        Output::<MinSig, DkgPk>::read_cfg(&mut &bytes[..], &(n, ModeVersion::v0()))
            .expect("undecodable previous Output")
    });
    Info::<MinSig, DkgPk>::new::<N3f1>(
        &dkg_namespace(&file.chain_id),
        file.round,
        previous,
        Mode::NonZeroCounter,
        Set::try_from(dealers).expect("dealers must be unique"),
        Set::try_from(players).expect("players must be unique"),
    )
    .expect("invalid DKG Info — check dealer/player sets and previous output")
}

fn load_info_file(path: &str) -> DkgInfoFile {
    load_json(path)
}

fn load_and_verify_requests(input_dir: &str) -> Vec<ShareRequest> {
    let mut requests: Vec<ShareRequest> = Vec::new();
    let entries =
        std::fs::read_dir(input_dir).unwrap_or_else(|e| panic!("Failed to read {input_dir}: {e}"));
    for entry in entries {
        let path = entry.expect("read_dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
        let req: ShareRequest = serde_json::from_str(&contents)
            .unwrap_or_else(|e| panic!("{} is not a valid share-request: {e}", path.display()));

        // Same attestation rule as assemble-genesis: `ed25519_pk` or
        // `ed25519_pk || mayo_pk` under the DKG namespace.
        let pk_bytes =
            hex::decode(&req.ed25519_public_hex).expect("share-request: bad ed25519_public_hex");
        let sig_bytes =
            hex::decode(&req.attestation_hex).expect("share-request: bad attestation_hex");
        let pk = decode_ed_pk(&req.ed25519_public_hex);
        let sig = ed25519::Signature::read_cfg(&mut &sig_bytes[..], &())
            .expect("share-request: undecodable attestation");
        let mut attest_msg = pk_bytes.clone();
        if let Some(mh) = &req.mayo_public_hex {
            let mpk = hex::decode(mh).expect("share-request: bad mayo_public_hex");
            if mpk.len() != 4912 {
                panic!(
                    "{}: mayo_public_hex must be a MAYO2 compact pk (4912 bytes)",
                    req.moniker
                );
            }
            attest_msg.extend_from_slice(&mpk);
        }
        if !pk.verify(DKG_ATTEST_NAMESPACE, &attest_msg, &sig) {
            panic!("{}: attestation verification failed", req.moniker);
        }
        requests.push(req);
    }
    requests.sort_by(|a, b| a.ed25519_public_hex.cmp(&b.ed25519_public_hex));
    for w in requests.windows(2) {
        if w[0].ed25519_public_hex == w[1].ed25519_public_hex {
            panic!(
                "duplicate ed25519 pubkey: {} and {}",
                w[0].moniker, w[1].moniker
            );
        }
    }
    requests
}

/// `dkg-info` — coordinator builds the round spec from share-requests.
/// Deterministic: every participant can rebuild and compare.
fn cmd_dkg_info(args: &[String]) {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys dkg-info --requests <dir> --chain-id <id> --round <n> [--previous <output.json>] [--out dkg-info.json] [--dealers <dir>]");
        println!("\nBuilds the round spec for a dealer-free DKG. --dealers restricts the");
        println!("dealer set to share-requests in a separate dir (resharing); defaults to");
        println!("the player set (fresh deal). --previous carries the prior Output.");
        return;
    }
    let input_dir = get_arg(args, "--requests")
        .unwrap_or("./share-requests")
        .to_string();
    let dealers_dir = get_arg(args, "--dealers").map(|s| s.to_string());
    let chain_id = get_arg(args, "--chain-id")
        .expect("--chain-id required")
        .to_string();
    let round: u64 = get_arg(args, "--round")
        .expect("--round required")
        .parse()
        .expect("--round must be a u64");
    let out = get_arg(args, "--out").unwrap_or("./dkg-info.json").to_string();

    let players = load_and_verify_requests(&input_dir);
    let dealer_hexes: Vec<String> = match &dealers_dir {
        Some(d) => load_and_verify_requests(d)
            .iter()
            .map(|r| r.ed25519_public_hex.clone())
            .collect(),
        None => players.iter().map(|r| r.ed25519_public_hex.clone()).collect(),
    };
    if players.len() < 4 {
        eprintln!(
            "Error: need at least 4 players for N3f1 consensus (got {})",
            players.len()
        );
        std::process::exit(1);
    }

    let previous_output_hex = get_arg(args, "--previous").map(|p| {
        let f: DkgOutputFile = load_json(&p);
        if f.round >= round {
            panic!("--round must exceed previous round {} (got {round})", f.round);
        }
        f.output_hex
    });

    let file = DkgInfoFile {
        chain_id,
        round,
        dealers: dealer_hexes,
        players: players.iter().map(|r| r.ed25519_public_hex.clone()).collect(),
        previous_output_hex,
    };
    // Construct the Info now so a malformed spec fails here, not mid-ceremony.
    build_info(&file);
    write_json(&out, &file);
    println!(
        "dkg-info written: {out}  ({} dealers → {} players, round {})",
        file.dealers.len(),
        file.players.len(),
        file.round
    );
    println!("Distribute this file to all participants; everyone runs dkg-deal.");
}

/// `dkg-deal` — each operator emits their public commitment + per-player
/// private share files. Deterministic under `dealer.seed`.
fn cmd_dkg_deal(args: &[String]) {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys dkg-deal --info dkg-info.json --keys keys.json [--out-dir ./dkg-out] [--plaintext-priv]");
        println!("\nCreates dealer.seed (32B secret — keep private until log publish),");
        println!("dealing-pub-<me>.json (broadcast) and dealing-priv-<me>-for-<peer>.json");
        println!("Each per-player file is sealed to the recipient's Ed25519 identity");
        println!("(X25519+XChaCha20-Poly1305) — safe to relay via a coordinator or");
        println!("public channel. --plaintext-priv emits the legacy cleartext format.");
        println!("Readers (dkg-ack/dkg-finalize) accept both formats.");
        return;
    }
    let info_path = get_arg(args, "--info")
        .unwrap_or("./dkg-info.json")
        .to_string();
    let keys_path = get_arg(args, "--keys").unwrap_or("./keys.json").to_string();
    let out_dir = get_arg(args, "--out-dir").unwrap_or("./dkg-out").to_string();
    // Per-player files are sealed to the recipient's Ed25519 identity by
    // default — the coordinator who ferries them never holds plaintext
    // shares. --plaintext-priv emits the legacy cleartext format for
    // interop/debug only.
    let plaintext_priv = has_flag(args, "--plaintext-priv");

    let file = load_info_file(&info_path);
    let info = build_info(&file);
    let (keys, sk) = load_identity(&keys_path);
    let me_hex = pk_hex(&sk.public_key());
    if !file.dealers.iter().any(|d| *d == me_hex) {
        eprintln!("Error: {me_hex} is not a dealer in this round.");
        std::process::exit(1);
    }

    // Prior share required when resharing.
    let prev_share: Option<Share> = file.previous_output_hex.as_ref().map(|_| {
        Share::read_cfg(
            &mut &hex::decode(
                keys.share_hex
                    .as_deref()
                    .expect("keys.json lacks share_hex — resharing needs a prior share"),
            )
            .expect("bad share_hex")[..],
            &(),
        )
        .expect("undecodable share")
    });

    std::fs::create_dir_all(&out_dir)
        .unwrap_or_else(|e| panic!("Failed to create {out_dir}: {e}"));
    let seed_path = format!("{out_dir}/dealer.seed");
    let seed: [u8; 32] = match std::fs::read(&seed_path) {
        Ok(b) if b.len() == 32 => b.try_into().unwrap(),
        Ok(_) => panic!("{seed_path} exists but is not 32 bytes"),
        Err(_) => {
            use rand::RngCore;
            let mut s = [0u8; 32];
            OsRng.fill_bytes(&mut s);
            std::fs::write(&seed_path, s)
                .unwrap_or_else(|e| panic!("Failed to write {seed_path}: {e}"));
            s
        }
    };

    let (_dealer, pub_msg, priv_msgs) = Dealer::<MinSig, _>::start::<N3f1>(
        ChaCha8Rng::from_seed(seed),
        info,
        sk,
        prev_share,
    )
    .expect("dealer start failed");

    let pub_file = DealingPubFile {
        dealer: me_hex.clone(),
        pub_msg_hex: hex::encode(pub_msg.encode().as_ref()),
    };
    write_json(
        &format!("{out_dir}/dealing-pub-{}.json", short(&me_hex)),
        &pub_file,
    );
    for (player, priv_msg) in &priv_msgs {
        let p_hex = pk_hex(player);
        let priv_bytes = priv_msg.encode().as_ref().to_vec();
        let f = if plaintext_priv {
            DealingPrivFile {
                dealer: me_hex.clone(),
                player: p_hex.clone(),
                pub_msg_hex: pub_file.pub_msg_hex.clone(),
                priv_msg_hex: Some(hex::encode(&priv_bytes)),
                enc: None,
            }
        } else {
            DealingPrivFile {
                dealer: me_hex.clone(),
                player: p_hex.clone(),
                pub_msg_hex: pub_file.pub_msg_hex.clone(),
                priv_msg_hex: None,
                enc: Some(encrypt_for_recipient(&p_hex, &priv_bytes)),
            }
        };
        write_json(
            &format!(
                "{out_dir}/dealing-priv-{}-for-{}.json",
                short(&me_hex),
                short(&p_hex)
            ),
            &f,
        );
    }
    println!("Dealing emitted (deterministic under {seed_path}):");
    println!("  broadcast: dealing-pub-{}.json", short(&me_hex));
    println!(
        "  deliver privately: dealing-priv-{}-for-<peer>.json",
        short(&me_hex)
    );
    println!("  keep dealer.seed until after dkg-log, then delete it.");
}

/// `dkg-ack` — each operator verifies the shares addressed to them and emits
/// signed acks per dealer. A bad share produces a complaint (no ack).
fn cmd_dkg_ack(args: &[String]) {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys dkg-ack --info dkg-info.json --keys keys.json --priv-dir <dir> [--out-dir ./acks]");
        println!("\n--priv-dir holds the dealing-priv-*-for-<me>.json files you received.");
        println!("Acks are public — broadcast them; each dealer needs its own.");
        return;
    }
    let info = build_info(&load_info_file(
        get_arg(args, "--info").unwrap_or("./dkg-info.json"),
    ));
    let (keys, sk) = load_identity(get_arg(args, "--keys").unwrap_or("./keys.json"));
    let my_seed = hex::decode(&keys.ed25519_private_hex).expect("bad ed25519_private_hex");
    let priv_dir = get_arg(args, "--priv-dir")
        .expect("--priv-dir required")
        .to_string();
    let out_dir = get_arg(args, "--out-dir").unwrap_or("./acks").to_string();

    let me = sk.public_key();
    let me_hex = pk_hex(&me);
    let mut player = Player::<MinSig, _>::new(info, sk).expect("not a player");
    let bound = NonZeroU32::new(10_000).unwrap(); // decode cap: commitment coeff count

    let mut acked = 0u32;
    let mut complaints = 0u32;
    for entry in std::fs::read_dir(&priv_dir).expect("read priv-dir") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with("dealing-priv-") || !name.ends_with(".json") {
            continue;
        }
        let f: DealingPrivFile = load_json(path.to_str().unwrap());
        if f.player != me_hex {
            continue; // not addressed to me — ignore (and do not peek).
        }
        let dealer = decode_ed_pk(&f.dealer);
        let pub_msg = DealerPubMsg::<MinSig>::read_cfg(
            &mut &hex::decode(&f.pub_msg_hex).expect("bad pub_msg_hex")[..],
            &bound,
        )
        .expect("undecodable DealerPubMsg");
        let priv_msg = DealerPrivMsg::read_cfg(
            &mut &resolve_priv_bytes(&f, &my_seed)[..],
            &(),
        )
        .expect("undecodable DealerPrivMsg");
        match player.dealer_message::<N3f1>(dealer, pub_msg, priv_msg) {
            Some(ack) => {
                let af = AckFile {
                    player: me_hex.clone(),
                    dealer: f.dealer.clone(),
                    ack_hex: hex::encode(ack.encode().as_ref()),
                };
                std::fs::create_dir_all(&out_dir).unwrap();
                let p = format!(
                    "{out_dir}/ack-{}-to-{}.json",
                    short(&me_hex),
                    short(&f.dealer)
                );
                write_json(&p, &af);
                println!("  acked dealer {}", short(&f.dealer));
                acked += 1;
            }
            None => {
                eprintln!(
                    "  COMPLAINT: share from dealer {} failed verification — do NOT ack; \
                     the dealer must publish a reveal in its log",
                    short(&f.dealer)
                );
                complaints += 1;
            }
        }
    }
    println!("{acked} acks written to {out_dir}, {complaints} complaints.");
    if complaints > 0 {
        println!("Wait for the offending dealer's log — it must reveal your share.");
    }
}

/// `dkg-log` — dealer replays received acks and publishes a signed log.
fn cmd_dkg_log(args: &[String]) {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys dkg-log --info dkg-info.json --keys keys.json --seed-file dealer.seed --acks-dir <dir> [--out-dir ./logs]");
        println!("\nRe-derives your dealer (seeded, deterministic), verifies each ack's");
        println!("signature, then emits log-<me>.json — broadcast it to all players.");
        return;
    }
    let file = load_info_file(get_arg(args, "--info").unwrap_or("./dkg-info.json"));
    let info = build_info(&file);
    let (keys, sk) = load_identity(get_arg(args, "--keys").unwrap_or("./keys.json"));
    let seed_path = get_arg(args, "--seed-file")
        .expect("--seed-file required")
        .to_string();
    let acks_dir = get_arg(args, "--acks-dir")
        .expect("--acks-dir required")
        .to_string();
    let out_dir = get_arg(args, "--out-dir").unwrap_or("./logs").to_string();

    let seed_b = std::fs::read(&seed_path)
        .unwrap_or_else(|e| panic!("Failed to read {seed_path} — run dkg-deal first: {e}"));
    let seed: [u8; 32] = seed_b.try_into().expect("dealer.seed must be 32 bytes");
    let prev_share: Option<Share> = file.previous_output_hex.as_ref().map(|_| {
        Share::read_cfg(
            &mut &hex::decode(keys.share_hex.clone().expect("missing share_hex"))
                .expect("bad share_hex")[..],
            &(),
        )
        .expect("undecodable share")
    });
    let (mut dealer, _pub_msg, _priv) = Dealer::<MinSig, _>::start::<N3f1>(
        ChaCha8Rng::from_seed(seed),
        info,
        sk.clone(),
        prev_share,
    )
    .expect("dealer rebuild failed");
    let me_hex = pk_hex(&sk.public_key());

    let mut accepted = 0u32;
    for entry in std::fs::read_dir(&acks_dir).expect("read acks-dir") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with("ack-") || !name.ends_with(".json") {
            continue;
        }
        let f: AckFile = load_json(path.to_str().unwrap());
        if f.dealer != me_hex {
            continue;
        }
        let player = decode_ed_pk(&f.player);
        let ack = PlayerAck::<DkgPk>::read_cfg(
            &mut &hex::decode(&f.ack_hex).expect("bad ack_hex")[..],
            &(),
        )
        .expect("undecodable PlayerAck");
        dealer
            .receive_player_ack(player, ack)
            .unwrap_or_else(|e| panic!("ack from {}: {e}", f.player));
        accepted += 1;
    }

    let signed = dealer.finalize::<N3f1>();
    std::fs::create_dir_all(&out_dir).unwrap();
    let out = format!("{out_dir}/log-{}.json", short(&me_hex));
    write_json(
        &out,
        &LogFile {
            dealer: me_hex.clone(),
            log_hex: hex::encode(signed.encode().as_ref()),
        },
    );
    println!("{accepted} acks incorporated → {out}");
    println!("Broadcast this log to all players; then run dkg-finalize.");
}

/// Load every `log-*.json`, check signatures, return the verified log map.
fn load_checked_logs(
    logs_dir: &str,
    info: &DkgInfo,
) -> BTreeMap<DkgPk, DealerLog<MinSig, DkgPk>> {
    let bound = NonZeroU32::new(10_000).unwrap();
    let mut logs = BTreeMap::new();
    for entry in std::fs::read_dir(logs_dir).expect("read logs-dir") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with("log-") || !name.ends_with(".json") {
            continue;
        }
        let f: LogFile = load_json(path.to_str().unwrap());
        let signed = SignedDealerLog::<MinSig, ed25519::PrivateKey>::read_cfg(
            &mut &hex::decode(&f.log_hex).expect("bad log_hex")[..],
            &bound,
        )
        .expect("undecodable SignedDealerLog");
        let (dealer, log) = signed
            .check(info)
            .unwrap_or_else(|| panic!("{}: dealer log signature invalid", f.dealer));
        if pk_hex(&dealer) != f.dealer {
            panic!(
                "{}: signed dealer key does not match filename/metadata",
                f.dealer
            );
        }
        logs.insert(dealer, log);
    }
    logs
}

/// `dkg-finalize` — each operator reconstructs their share from the public
/// logs + privately received dealings. Emits a bls-share.json identical in
/// shape to the coordinator-dealt package — `finalize` merges it unchanged.
fn cmd_dkg_finalize(args: &[String]) {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys dkg-finalize --info dkg-info.json --keys keys.json --priv-dir <dir> --logs-dir <dir> --requests <dir> [--out bls-share.json]");
        return;
    }
    let file = load_info_file(get_arg(args, "--info").unwrap_or("./dkg-info.json"));
    let info = build_info(&file);
    let (keys, sk) = load_identity(get_arg(args, "--keys").unwrap_or("./keys.json"));
    let my_seed = hex::decode(&keys.ed25519_private_hex).expect("bad ed25519_private_hex");
    let priv_dir = get_arg(args, "--priv-dir")
        .expect("--priv-dir required")
        .to_string();
    let logs_dir = get_arg(args, "--logs-dir")
        .expect("--logs-dir required")
        .to_string();
    let requests_dir = get_arg(args, "--requests")
        .expect("--requests required")
        .to_string();
    let out = get_arg(args, "--out").unwrap_or("./bls-share.json").to_string();

    let logs = load_checked_logs(&logs_dir, &info);
    let me = sk.public_key();
    let me_hex = pk_hex(&me);
    let mut player = Player::<MinSig, _>::new(info, sk).expect("not a player");
    let bound = NonZeroU32::new(10_000).unwrap();

    // Replay dealings — regenerates identical acks (Ed25519 is deterministic).
    for entry in std::fs::read_dir(&priv_dir).expect("read priv-dir") {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !name.starts_with("dealing-priv-") {
            continue;
        }
        let f: DealingPrivFile = load_json(path.to_str().unwrap());
        if f.player != me_hex {
            continue;
        }
        let pub_msg = DealerPubMsg::<MinSig>::read_cfg(
            &mut &hex::decode(&f.pub_msg_hex).expect("bad pub_msg_hex")[..],
            &bound,
        )
        .expect("undecodable DealerPubMsg");
        let priv_msg = DealerPrivMsg::read_cfg(
            &mut &resolve_priv_bytes(&f, &my_seed)[..],
            &(),
        )
        .expect("undecodable DealerPrivMsg");
        player.dealer_message::<N3f1>(decode_ed_pk(&f.dealer), pub_msg, priv_msg);
    }

    let (output, share) = player
        .finalize::<N3f1>(logs, &Sequential)
        .unwrap_or_else(|e| panic!("DKG finalize failed: {e}"));

    let sharing = output.public().clone();
    let requests = load_and_verify_requests(&requests_dir);
    let validator_index = requests
        .iter()
        .position(|r| r.ed25519_public_hex == me_hex)
        .expect("my pubkey not in the player set");
    let all_mayo = requests.iter().all(|r| r.mayo_public_hex.is_some());

    let package = BlsSharePackage {
        validator_index,
        share_index: share.index.get(),
        ed25519_public_hex: me_hex,
        sharing_hex: hex::encode(sharing.encode().as_ref()),
        share_hex: hex::encode(share.encode().as_ref()),
        threshold_public_key_hex: hex::encode(sharing.public().encode().as_ref()),
        threshold_required: sharing.required::<N3f1>(),
        threshold_total: sharing.total().get(),
        validator_public_keys: requests
            .iter()
            .map(|r| r.ed25519_public_hex.clone())
            .collect(),
        validator_mayo_public_keys: all_mayo
            .then(|| requests.iter().map(|r| r.mayo_public_hex.clone().unwrap()).collect()),
    };
    write_json(&out, &package);

    // Sanity: share's public half must match the polynomial at our index.
    let n_total = NonZeroU32::new(package.threshold_total).unwrap();
    let check = Sharing::<MinSig>::read_cfg(
        &mut &hex::decode(&package.sharing_hex).unwrap()[..],
        &(n_total, ModeVersion::v0()),
    )
    .unwrap();
    assert_eq!(
        check.partial_public(share.index).unwrap().encode().as_ref(),
        share.public::<MinSig>().encode().as_ref(),
        "share does not match group polynomial"
    );
    println!("Share reconstructed → {out}");
    println!(
        "  threshold: {}/{}",
        package.threshold_required, package.threshold_total
    );
    println!("  group key: {}...", &package.threshold_public_key_hex[..32]);
    println!("Run `finalize --keys keys.json --bls-share {out}` to merge.");
}

/// `dkg-observe` — anyone reconstructs the public Output from signed logs
/// only; writes shared.json in the same shape assemble-genesis emits.
fn cmd_dkg_observe(args: &[String]) {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys dkg-observe --info dkg-info.json --logs-dir <dir> --requests <dir> [--out-dir ./ceremony-out]");
        println!("\nPublic-material-only verification: derives the group key without any");
        println!("private share. Also writes output-<round>.json for future resharing.");
        return;
    }
    let file = load_info_file(get_arg(args, "--info").unwrap_or("./dkg-info.json"));
    let info = build_info(&file);
    let logs_dir = get_arg(args, "--logs-dir")
        .expect("--logs-dir required")
        .to_string();
    let requests_dir = get_arg(args, "--requests")
        .expect("--requests required")
        .to_string();
    let out_dir = get_arg(args, "--out-dir")
        .unwrap_or("./ceremony-out")
        .to_string();

    let logs = load_checked_logs(&logs_dir, &info);
    let output = observe::<MinSig, DkgPk, N3f1>(info, logs, &Sequential)
        .unwrap_or_else(|e| panic!("DKG observe failed: {e}"));
    let sharing = output.public().clone();

    let requests = load_and_verify_requests(&requests_dir);
    let threshold_public_key_hex = hex::encode(sharing.public().encode().as_ref());

    std::fs::create_dir_all(&out_dir).unwrap();
    let shared = SharedCeremonyOutput {
        chain_id: file.chain_id.clone(),
        threshold_public_key_hex: threshold_public_key_hex.clone(),
        threshold_required: sharing.required::<N3f1>(),
        threshold_total: sharing.total().get(),
        validators: requests
            .iter()
            .enumerate()
            .map(|(i, r)| SharedValidator {
                validator_index: i,
                moniker: r.moniker.clone(),
                p2p_address: r.p2p_address.clone(),
                ed25519_public_hex: r.ed25519_public_hex.clone(),
                mayo_public_hex: r.mayo_public_hex.clone(),
            })
            .collect(),
    };
    let shared_path = format!("{out_dir}/shared.json");
    write_json(&shared_path, &shared);

    let out_file = DkgOutputFile {
        chain_id: file.chain_id.clone(),
        round: file.round,
        output_hex: hex::encode(output.encode().as_ref()),
        threshold_public_key_hex,
        threshold_required: shared.threshold_required,
        threshold_total: shared.threshold_total,
    };
    let out_path = format!("{out_dir}/dkg-output-{}.json", file.round);
    write_json(&out_path, &out_file);

    println!("Observed Output (public material only):");
    println!("  shared.json:       {shared_path}");
    println!("  reshare output:    {out_path}  (pass via --previous next round)");
    println!("  group key:         {}...", &out_file.threshold_public_key_hex[..32]);
    if !output.revealed().is_empty() {
        println!(
            "  note: {} player share(s) were revealed in dealer logs",
            output.revealed().len()
        );
    }
}

/// `dkg-synthesize-genesis-output` — one-time bridge for the G1 set, whose
/// BLS key was coordinator-dealt (assemble-genesis/deal_anonymous) rather than
/// produced by a DKG round, so no prior `Output` exists for resharing.
///
/// Reconstructs a canonical round-0 `Output` encoding: the real public
/// polynomial (`sharing_hex` from any G1 keys.json), players = the sorted G1
/// validator set, dealers = same set, revealed = empty, and a deterministic
/// transcript summary binding it to the sharing + chain id. The result feeds
/// `dkg-info --previous`, letting the live set reshare WITHOUT rotating the
/// group public key — preserving downstream trust roots (IBC light client).
///
/// Soundness: the synthesized Output is only ever used to (a) bind the
/// reshare transcript and (b) check each dealer's share commitment against
/// `public.partial_public(players.index(dealer))` — both use only genuine
/// G1 public material. Dealer share scalars come from each operator's own
/// keys.json; nothing secret is synthesized here.
fn cmd_dkg_synthesize_genesis_output(args: &[String]) {
    if has_flag(args, "--help") || has_flag(args, "-h") {
        println!("Usage: generate-testnet-keys dkg-synthesize-genesis-output --keys <g1 keys.json> --shared <g1 shared.json> [--out dkg-output-0.json]");
        println!("\nBuilds the canonical round-0 Output for a coordinator-dealt genesis");
        println!("so the existing set can reshare without rotating the group key.");
        return;
    }
    let keys_path = get_arg(args, "--keys").expect("--keys required").to_string();
    let shared_path = get_arg(args, "--shared")
        .expect("--shared required")
        .to_string();
    let verify_dir = get_arg(args, "--verify-keys").map(|s| s.to_string());
    let out = get_arg(args, "--out")
        .unwrap_or("./dkg-output-0.json")
        .to_string();

    let keys: ValidatorKeys = load_json(&keys_path);
    let shared: SharedCeremonyOutput = load_json(&shared_path);

    // Players/dealers = the G1 validator set, sorted by pubkey (Set order).
    let mut pks: Vec<DkgPk> = shared
        .validators
        .iter()
        .map(|v| decode_ed_pk(&v.ed25519_public_hex))
        .collect();
    pks.sort();
    pks.dedup();
    if pks.len() != shared.validators.len() {
        panic!("shared.json contains duplicate validator pubkeys");
    }
    let players: Set<DkgPk> = Set::try_from(pks.clone()).expect("players must be unique");

    // Decode the genuine public polynomial dealt at genesis.
    let sharing_bytes = hex::decode(
        keys.sharing_hex
            .as_deref()
            .expect("keys.json lacks sharing_hex — is this a finalized G1 key?"),
    )
    .expect("keys.json: bad sharing_hex");
    let bound = NonZeroU32::new(10_000).unwrap();
    let sharing = Sharing::<MinSig>::read_cfg(
        &mut &sharing_bytes[..],
        &(bound, ModeVersion::v0()),
    )
    .expect("undecodable Sharing");
    assert_eq!(
        sharing.total().get() as usize,
        pks.len(),
        "sharing total ({}) != validator count ({})",
        sharing.total(),
        pks.len()
    );

    // Deterministic summary: binds this synthetic Output to chain-id + sharing.
    use commonware_cryptography::transcript::Transcript;
    let mut t = Transcript::new(b"junoclaw-genesis-output");
    t.commit(shared.chain_id.as_bytes());
    t.commit(sharing.encode());
    t.commit(players.encode());
    let summary_bytes: Vec<u8> = t.summarize().as_ref().to_vec();
    assert_eq!(summary_bytes.len(), 32);

    // Encode Output: summary || public || dealers || players || revealed.
    let empty: Set<DkgPk> = Set::try_from(Vec::<DkgPk>::new()).expect("empty set");
    let mut buf = Vec::new();
    buf.extend_from_slice(&summary_bytes);
    buf.extend_from_slice(&sharing.encode());
    buf.extend_from_slice(&players.encode()); // dealers = the genesis set
    buf.extend_from_slice(&players.encode()); // players = the genesis set
    buf.extend_from_slice(&empty.encode()); // no reveals

    // Round-trip through the real codec so a malformed synthesis fails here.
    let output = Output::<MinSig, DkgPk>::read_cfg(&mut &buf[..], &(bound, ModeVersion::v0()))
        .expect("synthesized Output failed to decode — bug");
    assert_eq!(
        output.public().public().encode().as_ref(),
        sharing.public().encode().as_ref(),
        "synthesized group key mismatch"
    );

    let group_hex = hex::encode(sharing.public().encode().as_ref());
    assert_eq!(
        group_hex, keys.threshold_public_key_hex,
        "sharing group key != keys.json threshold_public_key_hex"
    );

    let file = DkgOutputFile {
        chain_id: shared.chain_id.clone(),
        round: 0,
        output_hex: hex::encode(&buf),
        threshold_public_key_hex: group_hex,
        threshold_required: sharing.required::<N3f1>(),
        threshold_total: sharing.total().get(),
    };
    write_json(&out, &file);
    println!("Synthesized genesis Output (round 0):");
    println!("  out:      {out}");
    println!(
        "  players:  {} (threshold {}/{})",
        players.len(),
        file.threshold_required,
        file.threshold_total
    );
    println!("  group key: {}...  (unchanged — this is the whole point)", &file.threshold_public_key_hex[..32]);

    // Optional: verify each available G1 keys.json — the share's public half
    // must equal partial_public(position(pk)), exactly what Dealer::start
    // checks during resharing. Catches any index-ordering mismatch now.
    if let Some(dir) = verify_dir {
        let mut verified = 0u32;
        for entry in std::fs::read_dir(&dir).expect("read verify-keys dir") {
            let path = entry.unwrap().path();
            if path.extension().map(|e| e != "json").unwrap_or(true) {
                continue;
            }
            let k: ValidatorKeys = load_json(path.to_str().unwrap());
            let Some(share_hex) = k.share_hex.as_deref() else { continue };
            let share = Share::read_cfg(&mut &hex::decode(share_hex).expect("bad share_hex")[..], &())
                .expect("undecodable share");
            let pk = decode_ed_pk(&k.ed25519_public_hex);
            let participant = players
                .index(&pk)
                .unwrap_or_else(|| panic!("{} not in genesis player set", k.ed25519_public_hex));
            let expected = sharing
                .partial_public(participant)
                .expect("partial_public failed");
            assert_eq!(
                expected.encode().as_ref(),
                share.public::<MinSig>().encode().as_ref(),
                "share commitment mismatch for {} — index ordering wrong",
                k.ed25519_public_hex
            );
            verified += 1;
            println!(
                "  verified: {} (pos {}) share matches synthesized output",
                short(&k.ed25519_public_hex),
                participant.get()
            );
        }
        println!("  {verified} key share(s) verified against round-0 output");
    }
    println!("Next: dkg-info --requests <dir> --dealers <dir> --chain-id {} --round 1 --previous {out}", shared.chain_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity(seed: u64) -> (Vec<u8>, String) {
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let sk = ed25519::PrivateKey::random(&mut rng);
        (sk.encode().as_ref().to_vec(), pk_hex(&sk.public_key()))
    }

    #[test]
    fn enc_roundtrip() {
        let (seed, pub_hex) = test_identity(1);
        let plaintext = b"a-dealer-priv-msg-blob".to_vec();
        let enc = encrypt_for_recipient(&pub_hex, &plaintext);
        assert_eq!(enc.scheme, "x25519-xchacha20poly1305-v1");
        assert_eq!(decrypt_for_me(&enc, &seed), plaintext);
    }

    #[test]
    #[should_panic(expected = "dealing-priv decryption failed")]
    fn enc_wrong_recipient() {
        let (_seed_a, pub_a) = test_identity(1);
        let (seed_b, _pub_b) = test_identity(2);
        let enc = encrypt_for_recipient(&pub_a, b"secret".to_vec().as_ref());
        decrypt_for_me(&enc, &seed_b);
    }

    #[test]
    #[should_panic(expected = "dealing-priv decryption failed")]
    fn enc_tamper() {
        let (seed, pub_hex) = test_identity(1);
        let mut enc = encrypt_for_recipient(&pub_hex, b"secret".to_vec().as_ref());
        let mut ct = hex::decode(&enc.ct_hex).unwrap();
        ct[0] ^= 1;
        enc.ct_hex = hex::encode(ct);
        decrypt_for_me(&enc, &seed);
    }

    #[test]
    fn resolve_legacy_plaintext() {
        let f = DealingPrivFile {
            dealer: "aa".into(),
            player: "bb".into(),
            pub_msg_hex: "".into(),
            priv_msg_hex: Some(hex::encode(b"legacy")),
            enc: None,
        };
        assert_eq!(resolve_priv_bytes(&f, &[]), b"legacy");
    }

    #[test]
    fn resolve_encrypted_via_struct() {
        let (seed, pub_hex) = test_identity(7);
        let f = DealingPrivFile {
            dealer: "aa".into(),
            player: pub_hex.clone(),
            pub_msg_hex: "".into(),
            priv_msg_hex: None,
            enc: Some(encrypt_for_recipient(&pub_hex, b"sealed".to_vec().as_ref())),
        };
        assert_eq!(resolve_priv_bytes(&f, &seed), b"sealed");
    }

    #[test]
    fn file_serde_roundtrip() {
        let (_seed, pub_hex) = test_identity(3);
        let f = DealingPrivFile {
            dealer: "dd".into(),
            player: pub_hex.clone(),
            pub_msg_hex: "00".into(),
            priv_msg_hex: None,
            enc: Some(encrypt_for_recipient(&pub_hex, b"x".to_vec().as_ref())),
        };
        let json = serde_json::to_string_pretty(&f).unwrap();
        // Sealed files must not carry a plaintext field at all.
        assert!(!json.contains("priv_msg_hex"));
        assert!(json.contains("x25519-xchacha20poly1305-v1"));
        let back: DealingPrivFile = serde_json::from_str(&json).unwrap();
        assert!(back.enc.is_some());
        assert!(back.priv_msg_hex.is_none());
    }
}
