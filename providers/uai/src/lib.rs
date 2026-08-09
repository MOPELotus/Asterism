//! `uai` Provider implementation.
//!
//! The current Development checkpoint is a clean-room, fixture-only Course
//! resource and nested task-tree parser. It registers no runtime capability
//! and makes no claim of live compatibility.

mod course_inventory;
mod metadata;
mod task_inventory;

pub use course_inventory::{UaiCourseContext, parse_course_context, parse_course_inventory};
pub use metadata::development_metadata;
pub use task_inventory::parse_task_inventory;
