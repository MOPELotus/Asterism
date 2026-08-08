//! Chaoxing Provider implementation.
//!
//! The implementation contains deterministic Course, Work and Exam inventory
//! parsing plus an unverified native Work/Exam transport. It deliberately does
//! not claim live compatibility or a verified Provider.

mod course_inventory;
mod inventory;
mod metadata;
mod native_http;
mod task_inventory;

pub use course_inventory::{
    ChaoxingCourseInventory, ChaoxingCourseInventoryTransport, parse_course_inventory,
};
pub use inventory::{
    ChaoxingCourseScope, classify_work_detail, parse_exam_inventory, parse_work_inventory,
};
pub use native_http::{
    ChaoxingCookieSession, ChaoxingSessionResolver, NativeChaoxingInventoryTransport,
};
pub use task_inventory::{
    ChaoxingCourseRoute, ChaoxingInventoryDocument, ChaoxingInventoryTransport,
    ChaoxingTaskInventory,
};
