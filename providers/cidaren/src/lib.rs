//! `Cidaren` Provider implementation.
//!
//! The current Development checkpoint exposes manually imported token
//! authentication plus fixture-driven Course and class-task inventory through
//! injected transports. It makes no Capture, mutation or live-compatibility
//! claim.

mod authentication;
mod class_tasks;
mod inventory;
mod metadata;
mod provider;

pub use authentication::{
    CidarenAuthentication, CidarenAuthenticationTransport, CidarenSessionResolver,
    CidarenTokenSession, classify_token_validation_response,
};
pub use class_tasks::{parse_course_inventory, parse_task_inventory};
pub use inventory::{
    CidarenClassTaskPageDocument, CidarenClassTaskTransport, CidarenCourseInventory,
    CidarenTaskInventory,
};
pub use metadata::development_metadata;
pub use provider::build_development_provider;
