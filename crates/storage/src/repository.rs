use asterism_domain::{Task, TaskId, User, UserId};
use async_trait::async_trait;

use crate::StorageError;

/// Persistence contract consumed by task services. It intentionally contains no
/// `SQLite` types.
#[async_trait]
pub trait TaskRepository: Send + Sync {
    async fn find_task(&self, id: TaskId) -> Result<Option<Task>, StorageError>;
    async fn save_task(&self, task: &Task) -> Result<(), StorageError>;
}

/// Persistence contract consumed by identity and authorization services.
#[async_trait]
pub trait UserRepository: Send + Sync {
    async fn find_user(&self, id: UserId) -> Result<Option<User>, StorageError>;
    async fn save_user(&self, user: &User) -> Result<(), StorageError>;
}
