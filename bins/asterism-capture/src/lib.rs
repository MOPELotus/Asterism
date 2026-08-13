mod browser;
mod cdp_websocket;
mod client;
mod recipe;

pub use browser::ChromiumCapture;
pub use client::{
    CaptureClient, CaptureCredentialAccepted, CaptureCredentialField, CaptureCredentialSubmission,
    CaptureEventReceipt, CaptureHealth, ClaimedCaptureSession,
};
pub use recipe::{CaptureResolution, CaptureSnapshot};
