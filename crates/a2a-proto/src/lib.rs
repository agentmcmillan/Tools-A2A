// Re-export generated tonic/prost code.
// All proto types stay inside this crate; a2a-sdk and a2a-gateway
// expose clean domain types to callers.

pub mod local {
    tonic::include_proto!("local");
}

pub mod peer {
    tonic::include_proto!("peer");
}
