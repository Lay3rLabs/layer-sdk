pub(crate) fn timestamp_from_proto(
    proto: tendermint_proto::google::protobuf::Timestamp,
) -> cosmwasm_std::Timestamp {
    cosmwasm_std::Timestamp::from_seconds(proto.seconds as u64).plus_nanos(proto.nanos as u64)
}

#[allow(dead_code)]
pub(crate) fn timestamp_to_proto(
    std: cosmwasm_std::Timestamp,
) -> tendermint_proto::google::protobuf::Timestamp {
    tendermint_proto::google::protobuf::Timestamp {
        seconds: std.seconds().try_into().unwrap(),
        nanos: std.subsec_nanos().try_into().unwrap(),
    }
}

pub(crate) fn duration_from_proto(
    proto: tendermint_proto::google::protobuf::Duration,
) -> layer_std::Duration {
    layer_std::Duration::from_parts(proto.seconds as u64, proto.nanos as u64)
}

pub(crate) fn duration_to_proto(
    std: layer_std::Duration,
) -> tendermint_proto::google::protobuf::Duration {
    let (seconds, nanos) = std.to_parts();
    tendermint_proto::google::protobuf::Duration {
        seconds: seconds.try_into().unwrap(),
        nanos: nanos.try_into().unwrap(),
    }
}

// TODO: review if/when we can accept None values.
// For init stuff, making it all required for now.
pub(crate) fn consensus_params_from_proto(
    proto: tendermint_proto::types::ConsensusParams,
) -> layer_std::api::ConsensusParams {
    layer_std::api::ConsensusParams {
        block: block_params_from_proto(proto.block.unwrap()),
        evidence: evidence_params_from_proto(proto.evidence.unwrap()),
        version: proto.version.unwrap().app,
    }
}

pub(crate) fn block_params_from_proto(
    proto: tendermint_proto::types::BlockParams,
) -> layer_std::api::BlockParams {
    let max_gas = match proto.max_gas {
        0 | -1 => None,
        gas => Some(gas.try_into().unwrap()),
    };
    let max_bytes = proto.max_bytes.try_into().unwrap();

    layer_std::api::BlockParams { max_bytes, max_gas }
}

pub(crate) fn evidence_params_from_proto(
    proto: tendermint_proto::types::EvidenceParams,
) -> layer_std::api::EvidenceParams {
    let max_bytes = proto.max_bytes.try_into().unwrap();
    let max_age_blocks = proto.max_age_num_blocks.try_into().unwrap();
    let max_age_time = duration_from_proto(proto.max_age_duration.unwrap());

    layer_std::api::EvidenceParams {
        max_age_blocks,
        max_age_time,
        max_bytes,
    }
}

pub(crate) fn consensus_params_to_proto(
    std: layer_std::api::ConsensusParams,
) -> tendermint_proto::types::ConsensusParams {
    tendermint_proto::types::ConsensusParams {
        block: Some(block_params_to_proto(std.block)),
        evidence: Some(evidence_params_to_proto(std.evidence)),
        version: Some(tendermint_proto::types::VersionParams { app: std.version }),
        // TODO: do we need to add defaults here?
        validator: None,
        abci: None,
    }
}

pub(crate) fn block_params_to_proto(
    std: layer_std::api::BlockParams,
) -> tendermint_proto::types::BlockParams {
    let max_gas = std.max_gas.unwrap_or(0).try_into().unwrap();
    let max_bytes = std.max_bytes.try_into().unwrap();
    tendermint_proto::types::BlockParams { max_bytes, max_gas }
}

pub(crate) fn evidence_params_to_proto(
    std: layer_std::api::EvidenceParams,
) -> tendermint_proto::types::EvidenceParams {
    let max_bytes = std.max_bytes.try_into().unwrap();
    let max_age_num_blocks = std.max_age_blocks.try_into().unwrap();
    let max_age_duration = Some(duration_to_proto(std.max_age_time));

    tendermint_proto::types::EvidenceParams {
        max_age_num_blocks,
        max_age_duration,
        max_bytes,
    }
}

pub(crate) fn validator_updates_from_proto(
    proto: Vec<tendermint_proto::abci::ValidatorUpdate>,
) -> Vec<layer_std::api::ValidatorUpdate> {
    proto.into_iter().map(validator_update_from_proto).collect()
}

pub(crate) fn validator_update_from_proto(
    proto: tendermint_proto::abci::ValidatorUpdate,
) -> layer_std::api::ValidatorUpdate {
    layer_std::api::ValidatorUpdate {
        pub_key: decode_tm_pubkey(proto.pub_key.unwrap()),
        power: proto.power.try_into().unwrap(),
    }
}

pub(crate) fn validator_updates_to_proto(
    std: Vec<layer_std::api::ValidatorUpdate>,
) -> Vec<tendermint_proto::abci::ValidatorUpdate> {
    std.into_iter().map(validator_update_to_proto).collect()
}

pub(crate) fn validator_update_to_proto(
    std: layer_std::api::ValidatorUpdate,
) -> tendermint_proto::abci::ValidatorUpdate {
    tendermint_proto::abci::ValidatorUpdate {
        pub_key: Some(encode_tm_pubkey(std.pub_key)),
        power: std.power as i64,
    }
}

pub(crate) fn encode_tm_pubkey(
    pubkey: layer_std::api::TmPubKey,
) -> tendermint_proto::crypto::PublicKey {
    let sum = match pubkey {
        layer_std::api::TmPubKey::Ed25519(pk) => {
            tendermint_proto::crypto::public_key::Sum::Ed25519(pk)
        }
        layer_std::api::TmPubKey::Secp256k1(pk) => {
            tendermint_proto::crypto::public_key::Sum::Secp256k1(pk)
        }
    };
    tendermint_proto::crypto::PublicKey { sum: Some(sum) }
}

pub(crate) fn decode_tm_pubkey(
    pubkey: tendermint_proto::crypto::PublicKey,
) -> layer_std::api::TmPubKey {
    match pubkey.sum.unwrap() {
        tendermint_proto::crypto::public_key::Sum::Ed25519(pk) => {
            layer_std::api::TmPubKey::Ed25519(pk)
        }
        tendermint_proto::crypto::public_key::Sum::Secp256k1(pk) => {
            layer_std::api::TmPubKey::Secp256k1(pk)
        }
    }
}

pub(crate) fn events_to_proto(
    event: Vec<cosmwasm_std::Event>,
) -> Vec<tendermint_proto::abci::Event> {
    event.into_iter().map(event_to_proto).collect()
}

pub(crate) fn event_to_proto(event: cosmwasm_std::Event) -> tendermint_proto::abci::Event {
    let attributes = event
        .attributes
        .into_iter()
        .map(|a| tendermint_proto::abci::EventAttribute {
            key: a.key,
            value: a.value,
            index: true,
        })
        .collect();
    tendermint_proto::abci::Event {
        r#type: event.ty,
        attributes,
    }
}
