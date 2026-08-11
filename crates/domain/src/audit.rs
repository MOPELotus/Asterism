use serde::{Deserialize, Serialize};

use crate::{AuditRecordId, Timestamp};

/// Immutable sanitized operational audit fact. Actor/resource labels remain
/// extensible strings because workers and bootstrap sessions are valid actors
/// in addition to users and service tokens.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AuditRecord {
    pub id: AuditRecordId,
    pub occurred_at: Timestamp,
    pub actor_type: String,
    pub actor_id: Option<String>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub request_id: Option<String>,
    pub correlation_id: Option<String>,
    pub outcome: String,
    pub metadata_sanitized: serde_json::Value,
}
