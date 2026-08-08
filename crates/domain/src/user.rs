use serde::{Deserialize, Serialize};

use crate::{Timestamp, UserId};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    Active,
    Suspended,
    Disabled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Master,
    Operator,
    User,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    ReadProviders,
    ReadOwnTasks,
    ManageUsers,
    ManageProviders,
    ManageCredits,
    GrantCredits,
    ManagePricing,
    ManageSystem,
    ManageOwnAccounts,
    ReadOwnCredits,
    ExecuteOwnTasks,
    ExecuteAnyTask,
    ViewOwnAudit,
    ViewAnyAudit,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct User {
    pub id: UserId,
    pub username: String,
    /// PHC-formatted Argon2id hash; plaintext passwords never enter this model.
    pub password_hash: String,
    pub status: UserStatus,
    pub roles: Vec<Role>,
    pub permissions: Vec<Permission>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QqIdentity {
    pub user_id: UserId,
    pub qq: u64,
    pub verified_at: Timestamp,
    pub primary: bool,
}
