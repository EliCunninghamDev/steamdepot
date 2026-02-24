#[derive(Clone)]
pub struct SessionState {
    pub steam_id: u64,
    pub session_id: i32,
    pub cell_id: u32,
    pub heartbeat_seconds: i32,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            steam_id: 0,
            session_id: 0,
            cell_id: 0,
            heartbeat_seconds: 0,
        }
    }
}
