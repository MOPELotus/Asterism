mod browser;
mod cdp_websocket;
mod client;
mod provider_bridge;
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
pub use provider_bridge::{BrowserBridgeProviderResult, handle_cidaren_browser_command};
pub use recipe::{CaptureResolution, CaptureSnapshot};
