//! `WELearn` Provider implementation.
//!
//! The current Development checkpoint provides native Authentication,
//! Course/Task inventory and read-only CMI progress behind explicit daemon
//! opt-in. Its parser/native-boundary coverage makes no claim of live
//! compatibility.

mod authentication;
mod cmi;
mod course_context;
mod course_inventory;
mod duration_report;
mod inventory_capabilities;
mod metadata;
mod native_authentication;
mod native_http;
mod provider;
mod runtime_settings;
mod stored_session;
mod task_detail;
mod task_inventory;

pub use authentication::{
    WellearnAuthentication, WellearnAuthenticationTransport, WellearnCookieSession,
    WellearnLoginRedirect, WellearnPasswordCipher, WellearnSessionResolver,
    classify_password_login_response, encode_password_at,
};
pub use cmi::{
    WellearnCmiDocument, WellearnCmiSnapshot, WellearnCmiTransport, WellearnTaskProgress,
    parse_cmi_snapshot,
};
pub use course_context::{WellearnCourseContext, parse_course_context};
pub use course_inventory::parse_course_inventory;
pub use duration_report::{
    WellearnDurationReport, WellearnDurationReportDocuments, WellearnDurationReportTransport,
};
pub use inventory_capabilities::{
    WellearnCourseInventory, WellearnCourseInventoryTransport, WellearnInventoryDocument,
    WellearnTaskInventory, WellearnTaskInventoryDocuments, WellearnTaskInventoryTransport,
};
pub use metadata::development_metadata;
pub use native_authentication::NativeWellearnAuthenticationTransport;
pub use native_http::NativeWellearnInventoryTransport;
pub use provider::{
    build_development_provider, build_development_provider_native,
    build_development_provider_with_native_inventory, build_development_provider_with_renewal,
    build_development_provider_with_stored_session,
};
pub use stored_session::StoredWellearnSessionResolver;
pub use task_detail::WellearnTaskDetail;
pub use task_inventory::{WellearnScoLeavesDocument, parse_task_inventory};
