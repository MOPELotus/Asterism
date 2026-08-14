mod browser;
mod cdp_websocket;
mod client;
mod recipe;

pub use browser::{
    BrowserBridgeDocumentBinding, BrowserBridgeReadSnapshot, ChromiumBrowserBridge, ChromiumCapture,
};
pub use client::{
    BrowserBridgeCommand, BrowserBridgeResultReceipt, BrowserBridgeRuntimeBindingReceipt,
    BrowserBridgeSessionSnapshot, CaptureClient, CaptureCredentialAccepted, CaptureCredentialField,
    CaptureCredentialSubmission, CaptureEventReceipt, CaptureHealth, ClaimedBrowserBridgeSession,
    ClaimedCaptureSession,
};
pub use recipe::{CaptureResolution, CaptureSnapshot};
