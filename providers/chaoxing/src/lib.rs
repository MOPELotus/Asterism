//! Chaoxing Provider implementation.
//!
//! The first slice contains only deterministic, offline inventory parsing. It
//! deliberately does not claim a live transport or a verified Provider.

mod inventory;
mod task_inventory;

pub use inventory::{
    ChaoxingCourseScope, classify_work_detail, parse_exam_inventory, parse_work_inventory,
};
pub use task_inventory::{
    ChaoxingCourseRoute, ChaoxingInventoryDocument, ChaoxingInventoryTransport,
    ChaoxingTaskInventory,
};
