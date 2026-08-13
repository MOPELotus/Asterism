use asterism_domain::{AuthMethod, SessionKind};
use asterism_provider_api::{
    CaptureCredentialOutput, CaptureJsonField, CaptureRecipe, CaptureScalarSource,
    CaptureValueSource,
};
use asterism_secrets::SecretPurpose;

const ORIGIN: &str = "https://app.vocabgo.com";

/// Returns the exact current donor-observed Cidaren Capture recipe.
///
/// Both required outputs are taken from one logical browser snapshot. The
/// Composite JSON retains the browser session observation alongside
/// `login_info`; Cidaren crypto consumes only the audited login-info fields.
pub fn cidaren_capture_recipe_v1() -> CaptureRecipe {
    CaptureRecipe {
        version: 1,
        start_url: "https://app.vocabgo.com/student/".to_owned(),
        allowed_origins: vec![ORIGIN.to_owned()],
        poll_interval_millis: 800,
        auth_method: AuthMethod::AssistedSession,
        session_kind: SessionKind::Composite,
        outputs: vec![
            CaptureCredentialOutput {
                purpose: SecretPurpose::ProviderAccessToken,
                required: true,
                sources: vec![
                    CaptureValueSource::RequestHeader {
                        origin: ORIGIN.to_owned(),
                        name: "UserToken".to_owned(),
                    },
                    CaptureValueSource::LocalStorage {
                        origin: ORIGIN.to_owned(),
                        key: "CDR_USER_TOKEN".to_owned(),
                    },
                    CaptureValueSource::SessionStorage {
                        origin: ORIGIN.to_owned(),
                        key: "CDR_USER_TOKEN".to_owned(),
                    },
                ],
            },
            CaptureCredentialOutput {
                purpose: SecretPurpose::ProviderCompositeSession,
                required: true,
                sources: vec![CaptureValueSource::JsonObject {
                    fields: vec![
                        CaptureJsonField {
                            name: "login_info".to_owned(),
                            sources: vec![
                                CaptureScalarSource::LocalStorage {
                                    origin: ORIGIN.to_owned(),
                                    key: "CDR_LOGIN_INFO".to_owned(),
                                },
                                CaptureScalarSource::SessionStorage {
                                    origin: ORIGIN.to_owned(),
                                    key: "CDR_LOGIN_INFO".to_owned(),
                                },
                            ],
                        },
                        CaptureJsonField {
                            name: "user_session".to_owned(),
                            sources: vec![
                                CaptureScalarSource::LocalStorage {
                                    origin: ORIGIN.to_owned(),
                                    key: "CDR_USER_SESSION".to_owned(),
                                },
                                CaptureScalarSource::SessionStorage {
                                    origin: ORIGIN.to_owned(),
                                    key: "CDR_USER_SESSION".to_owned(),
                                },
                            ],
                        },
                    ],
                }],
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_recipe_is_valid_origin_bounded_and_atomically_complete() {
        let recipe = cidaren_capture_recipe_v1();
        recipe.validate().unwrap();
        assert_eq!(recipe.version, 1);
        assert_eq!(recipe.start_url, "https://app.vocabgo.com/student/");
        assert_eq!(recipe.allowed_origins, [ORIGIN]);
        assert_eq!(recipe.poll_interval_millis, 800);
        assert_eq!(recipe.auth_method, AuthMethod::AssistedSession);
        assert_eq!(recipe.session_kind, SessionKind::Composite);
        assert_eq!(recipe.outputs.len(), 2);
        assert!(recipe.outputs.iter().all(|output| output.required));
        assert_eq!(
            recipe
                .outputs
                .iter()
                .map(|output| output.purpose)
                .collect::<Vec<_>>(),
            [
                SecretPurpose::ProviderAccessToken,
                SecretPurpose::ProviderCompositeSession,
            ]
        );
        let encoded = serde_json::to_string(&recipe).unwrap();
        assert!(!encoded.to_ascii_lowercase().contains("script"));
        assert!(!encoded.to_ascii_lowercase().contains("proxy"));
    }
}
