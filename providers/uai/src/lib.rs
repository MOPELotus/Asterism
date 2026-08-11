//! `uai` Provider implementation.
//!
//! The current Development checkpoint provides native Password/JWT
//! authentication, read-only Course/Task inventory, fresh Group detail,
//! Group progress and identity-bound duration over the shared network policy.
//! It makes no claim of live compatibility.

mod annotator;
mod authentication;
mod course_inventory;
mod duration;
mod inventory_capabilities;
mod metadata;
mod native_authentication;
mod native_http;
mod progress;
mod provider;
mod stored_session;
mod task_detail;
mod task_inventory;
mod user_identity;

pub use authentication::{
    UaiAuthentication, UaiAuthenticationTransport, UaiJwtSession, UaiSessionResolver,
    classify_password_login_response,
};
pub use course_inventory::{UaiCourseContext, parse_course_context, parse_course_inventory};
pub use duration::{
    UaiDurationDocument, UaiDurationTransport, UaiTaskDuration, parse_task_duration,
};
pub use inventory_capabilities::{
    UaiCourseInventory, UaiCourseInventoryTransport, UaiInventoryDocument, UaiTaskInventory,
    UaiTaskInventoryDocuments, UaiTaskInventoryTransport,
};
pub use metadata::development_metadata;
pub use native_authentication::NativeUaiAuthenticationTransport;
pub use native_http::NativeUaiInventoryTransport;
pub use progress::{
    UaiGroupProgressSnapshot, UaiProgressDocument, UaiProgressTransport, UaiTaskProgress,
    parse_group_progress,
};
pub use provider::{
    build_development_provider, build_development_provider_native,
    build_development_provider_with_native_inventory, build_development_provider_with_renewal,
    build_development_provider_with_stored_session,
};
pub use stored_session::StoredUaiSessionResolver;
pub use task_detail::UaiTaskDetail;
pub use task_inventory::parse_task_inventory;
