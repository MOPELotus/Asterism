//! Chaoxing Provider implementation.
//!
//! The first slice contains only deterministic, offline inventory parsing. It
//! deliberately does not claim a live transport or a verified Provider.

mod inventory;

pub use inventory::{
    ChaoxingCourseScope, classify_work_detail, parse_exam_inventory, parse_work_inventory,
};
