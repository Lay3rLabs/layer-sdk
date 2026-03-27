pub mod cosmos {
    pub mod auth {
        pub mod v1beta1 {
            include!("protos/cosmos.auth.v1beta1.rs");
        }
    }

    pub mod bank {
        pub mod v1beta1 {
            include!("protos/cosmos.bank.v1beta1.rs");
        }
    }

    pub mod base {
        pub mod v1beta1 {
            include!("protos/cosmos.base.v1beta1.rs");
        }

        pub mod abci {
            pub mod v1beta1 {
                include!("protos/cosmos.base.abci.v1beta1.rs");
            }
        }

        pub mod query {
            pub mod v1beta1 {
                include!("protos/cosmos.base.query.v1beta1.rs");
            }
        }

        pub mod tendermint {
            pub mod v1beta1 {
                include!("protos/cosmos.base.tendermint.v1beta1.rs");
            }
        }
    }

    pub mod crypto {
        pub mod secp256k1 {
            include!("protos/cosmos.crypto.secp256k1.rs");
        }

        pub mod ed25519 {
            include!("protos/cosmos.crypto.ed25519.rs");
        }
    }

    pub mod tx {
        pub mod v1beta1 {
            include!("protos/cosmos.tx.v1beta1.rs");
        }

        pub mod signing {
            pub mod v1beta1 {
                include!("protos/cosmos.tx.signing.v1beta1.rs");
            }
        }
    }
}

pub mod cosmwasm {
    pub mod wasm {
        pub mod v1 {
            include!("protos/cosmwasm.wasm.v1.rs");
        }
    }
}

pub mod google {
    #[allow(clippy::doc_lazy_continuation)]
    pub mod api {
        include!("protos/google.api.rs");
    }

    pub mod protobuf {
        include!("protos/google.protobuf.rs");
    }
}

pub mod tendermint {
    #[allow(clippy::large_enum_variant)]
    pub mod abci {
        include!("protos/tendermint.abci.rs");
    }

    pub mod crypto {
        include!("protos/tendermint.crypto.rs");
    }

    pub mod p2p {
        include!("protos/tendermint.p2p.rs");
    }

    #[allow(clippy::large_enum_variant)]
    pub mod types {
        include!("protos/tendermint.types.rs");
    }

    pub mod version {
        include!("protos/tendermint.version.rs");
    }
}

pub mod layer {
    pub mod sync {
        pub mod v1 {
            include!("protos/layer.sync.v1.rs");
        }
    }
}
