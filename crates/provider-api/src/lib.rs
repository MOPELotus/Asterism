//! Capability-based contracts implemented by Asterism providers.

mod capability;
mod error;
mod metadata;
mod registry;
mod settings;

pub use capability::*;
pub use error::{ProviderError, ProviderErrorKind, ProviderResult};
pub use metadata::{ProviderCapability, ProviderMetadata, VerificationLevel};
pub use registry::{ProviderEntry, ProviderRegistry, RegistryError};
pub use settings::{
    ProviderRuntimeSettingsPatch, ProviderRuntimeSettingsSchema, ProviderSettingDefinition,
    ProviderSettingKind, ProviderSettingScope, ProviderSettingValue, ProviderSettingsError,
    ResolvedProviderRuntimeSettings,
};
