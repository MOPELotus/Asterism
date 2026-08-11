//! `Cidaren` Provider implementation.
//!
//! The current Development checkpoint exposes manually imported token
//! authentication plus fixture/native-boundary class/ordinary-study Course and
//! Task inventories with fresh detail/progress reads through Core-scoped
//! stored tokens. It makes no Capture, mutation or live-compatibility claim.

mod authentication;
mod class_tasks;
mod inventory;
mod metadata;
mod native_http;
mod provider;
mod stored_session;
mod study_tasks;
mod task_read;

pub use authentication::{
    CidarenAuthentication, CidarenAuthenticationTransport, CidarenSessionResolver,
    CidarenTokenSession, classify_token_validation_response,
};
pub use class_tasks::{parse_course_inventory, parse_task_inventory};
pub use inventory::{
    CidarenClassTaskPageDocument, CidarenClassTaskTransport, CidarenCourseInventory,
    CidarenStudyTaskDocument, CidarenStudyTaskTransport, CidarenTaskInventory,
};
pub use metadata::development_metadata;
pub use native_http::NativeCidarenTransport;
pub use provider::{
    build_development_provider, build_development_provider_native,
    build_development_provider_with_stored_session,
};
pub use stored_session::StoredCidarenSessionResolver;
pub use study_tasks::{parse_study_course, parse_study_task_inventory};
pub use task_read::{CidarenTaskDetail, CidarenTaskProgress};
