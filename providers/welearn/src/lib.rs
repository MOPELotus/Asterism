//! `WELearn` Provider implementation.
//!
//! The current checkpoint contains clean-room, fixture-only Course and SCO
//! inventory parsers. It deliberately registers no runtime capability and
//! makes no claim of live compatibility.

mod authentication;
mod course_inventory;
mod metadata;
mod task_inventory;

pub use authentication::{
    WellearnAuthentication, WellearnAuthenticationTransport, WellearnCookieSession,
    WellearnLoginRedirect, WellearnPasswordCipher, WellearnSessionResolver,
    classify_password_login_response, encode_password_at,
};
pub use course_inventory::parse_course_inventory;
pub use metadata::development_metadata;
pub use task_inventory::{WellearnScoLeavesDocument, parse_task_inventory};
