//! `uai` Provider implementation.
//!
//! The current Development checkpoint provides bounded offline Course/task
//! parsers plus native Password/JWT authentication over the shared network
//! policy. It makes no claim of live compatibility.

mod authentication;
mod course_inventory;
mod metadata;
mod native_authentication;
mod stored_session;
mod task_inventory;

pub use authentication::{
    UaiAuthentication, UaiAuthenticationTransport, UaiJwtSession, UaiSessionResolver,
    classify_password_login_response,
};
pub use course_inventory::{UaiCourseContext, parse_course_context, parse_course_inventory};
pub use metadata::development_metadata;
pub use native_authentication::NativeUaiAuthenticationTransport;
pub use stored_session::StoredUaiSessionResolver;
pub use task_inventory::parse_task_inventory;
