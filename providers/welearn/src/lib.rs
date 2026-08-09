//! `WELearn` Provider implementation.
//!
//! The current checkpoint contains clean-room, fixture-only Course and SCO
//! inventory parsers. It deliberately registers no runtime capability and
//! makes no claim of live compatibility.

mod authentication;
mod course_context;
mod course_inventory;
mod inventory_capabilities;
mod metadata;
mod native_http;
mod provider;
mod task_inventory;

pub use authentication::{
    WellearnAuthentication, WellearnAuthenticationTransport, WellearnCookieSession,
    WellearnLoginRedirect, WellearnPasswordCipher, WellearnSessionResolver,
    classify_password_login_response, encode_password_at,
};
pub use course_context::{WellearnCourseContext, parse_course_context};
pub use course_inventory::parse_course_inventory;
pub use inventory_capabilities::{
    WellearnCourseInventory, WellearnCourseInventoryTransport, WellearnInventoryDocument,
    WellearnTaskInventory, WellearnTaskInventoryDocuments, WellearnTaskInventoryTransport,
};
pub use metadata::development_metadata;
pub use native_http::NativeWellearnInventoryTransport;
pub use provider::{build_development_provider, build_development_provider_with_native_inventory};
pub use task_inventory::{WellearnScoLeavesDocument, parse_task_inventory};
