use tonic::metadata::MetadataMap;

pub mod actions;
pub mod conversation;
pub mod image_store;
pub mod vision;

/// Read the `x-ai-mic-run-id` gRPC header, which identifies each conversation execution
pub fn extract_run_id(metadata: &MetadataMap) -> String {
    metadata
        .get("x-ai-mic-run-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string()
}
