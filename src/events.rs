use serde::Serialize;
use tokio::sync::broadcast;

pub const CHANNEL_CAPACITY: usize = 512;

pub type EventTx = broadcast::Sender<AppEvent>;

#[derive(Clone, Debug, Serialize)]
pub struct AppEvent {
    pub ts: String,
    pub level: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    pub msg: String,
    pub data: serde_json::Value,
}

pub fn new_channel() -> EventTx {
    let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
    tx
}

pub fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

pub fn emit(
    tx: &EventTx,
    level: &str,
    kind: &str,
    job_id: Option<&str>,
    ip: Option<&str>,
    msg: &str,
    data: serde_json::Value,
) {
    if tx.receiver_count() == 0 {
        return;
    }
    let event = AppEvent {
        ts: now_rfc3339(),
        level: level.to_string(),
        kind: kind.to_string(),
        job_id: job_id.map(str::to_owned),
        ip: ip.map(str::to_owned),
        msg: msg.to_string(),
        data,
    };
    let _ = tx.send(event);
}
