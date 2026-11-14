pub mod api;
pub mod archive;
pub mod collect;
pub mod erase;
pub mod evac;
pub mod mcp;
pub mod semantic;
pub mod store;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
