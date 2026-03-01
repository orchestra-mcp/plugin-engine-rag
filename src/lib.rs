pub mod db;
pub mod index;
pub mod lsp;
pub mod memory;
pub mod parser;
pub mod protocol;
pub mod tools;

/// Generated Protobuf types from orchestra/plugin/v1/plugin.proto.
/// These are prost-generated structs (NOT gRPC services).
pub mod proto {
    pub mod orchestra {
        pub mod plugin {
            pub mod v1 {
                include!(concat!(env!("OUT_DIR"), "/orchestra.plugin.v1.rs"));
            }
        }
    }
}
