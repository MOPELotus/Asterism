//! `uai` Provider implementation.
//!
//! The current Development checkpoint provides native Password/JWT
//! authentication and read-only Course/Task inventory over the shared network
//! policy. It makes no claim of live compatibility.

mod authentication;
mod course_inventory;
mod inventory_capabilities;
mod metadata;
mod native_authentication;
mod native_http;
mod stored_session;
mod task_inventory;

pub use authentication::{
    UaiAuthentication, UaiAuthenticationTransport, UaiJwtSession, UaiSessionResolver,
    classify_password_login_response,
};
pub use course_inventory::{UaiCourseContext, parse_course_context, parse_course_inventory};
pub use inventory_capabilities::{
    UaiCourseInventory, UaiCourseInventoryTransport, UaiInventoryDocument, UaiTaskInventory,
    UaiTaskInventoryDocuments, UaiTaskInventoryTransport,
};
pub use metadata::development_metadata;
pub use native_authentication::NativeUaiAuthenticationTransport;
pub use native_http::NativeUaiInventoryTransport;
pub use stored_session::StoredUaiSessionResolver;
pub use task_inventory::parse_task_inventory;
