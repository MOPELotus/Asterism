//! Chaoxing Provider implementation.
//!
//! The implementation contains deterministic authentication, Course, Work and
//! Exam contracts plus unverified native transports. It deliberately does not
//! claim live compatibility or a verified Provider.

mod authentication;
mod chapter_inventory;
mod course_inventory;
mod inventory;
mod metadata;
mod native_http;
mod provider;
mod resource_inventory;
mod stored_session;
mod task_inventory;

pub use authentication::{
    ChaoxingAuthentication, ChaoxingAuthenticationTransport, NativeChaoxingAuthenticationTransport,
};
pub use chapter_inventory::parse_chapter_inventory;
pub use course_inventory::{
    ChaoxingCourseInventory, ChaoxingCourseInventoryTransport, parse_course_inventory,
};
pub use inventory::{
    ChaoxingCourseScope, classify_work_detail, parse_exam_inventory, parse_work_inventory,
};
pub use native_http::{
    ChaoxingCookieSession, ChaoxingSessionResolver, NativeChaoxingInventoryTransport,
};
pub use provider::{build_development_provider, build_development_provider_with_renewal};
pub use resource_inventory::parse_chapter_resource_inventory;
pub use stored_session::StoredChaoxingSessionResolver;
pub use task_inventory::{
    ChaoxingChapterResourceDocument, ChaoxingChapterResourceRequest, ChaoxingCourseRoute,
    ChaoxingInventoryDocument, ChaoxingInventoryTransport, ChaoxingTaskInventory,
    ChaoxingWorkDetailRequest, ChaoxingWorkDetailState,
};
