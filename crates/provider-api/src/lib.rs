//! Capability-based contracts implemented by Asterism providers.

mod capability;
mod course_enrollment;
mod error;
mod history_harvest;
mod metadata;
mod registry;
mod settings;

pub use capability::*;
pub use course_enrollment::*;
pub use error::{ProviderError, ProviderErrorKind, ProviderProtocolObservation, ProviderResult};
pub use history_harvest::*;
pub use metadata::{ProviderCapability, ProviderMetadata, VerificationLevel};
pub use registry::{ProviderEntry, ProviderRegistry, RegistryError};
pub use settings::{
    ProviderExecutionConcurrency, ProviderRuntimeSettingSource, ProviderRuntimeSettingsPatch,
    ProviderRuntimeSettingsSchema, ProviderSettingCoreBehavior, ProviderSettingDefinition,
    ProviderSettingKind, ProviderSettingScope, ProviderSettingValue, ProviderSettingsError,
    ResolvedProviderRuntimeSettings,
};
