use asterism_domain::{AuthMethod, SessionKind};
use asterism_provider_api::{
    CaptureCredentialOutput, CaptureJsonField, CaptureReadiness, CaptureRecipe,
    CaptureScalarSource, CaptureValueSource,
};
use asterism_secrets::SecretPurpose;

const ORIGIN: &str = "https://app.vocabgo.com";

/// Returns the exact current donor-observed Cidaren Capture recipe.
///
/// Both required outputs are taken from one logical browser snapshot. The
/// donor can observe `CDR_USER_SESSION`, but its completion gate and `jv=99`
/// crypto consume only `CDR_LOGIN_INFO`; keeping the unused optional value out
/// of the required JSON object prevents a valid capture from waiting forever
/// and avoids storing unnecessary browser state.
pub fn cidaren_capture_recipe_v2() -> CaptureRecipe {
    CaptureRecipe {
        version: 2,
        start_url: "https://app.vocabgo.com/student/".to_owned(),
        navigation_origins: vec![ORIGIN.to_owned()],
        read_origins: vec![ORIGIN.to_owned()],
        poll_interval_millis: 800,
        auth_method: AuthMethod::AssistedSession,
        session_kind: SessionKind::Composite,
        readiness: CaptureReadiness::OutputsComplete,
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
                    fields: vec![CaptureJsonField {
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
                    }],
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
        let recipe = cidaren_capture_recipe_v2();
        recipe.validate().unwrap();
        assert_eq!(recipe.version, 2);
        assert_eq!(recipe.start_url, "https://app.vocabgo.com/student/");
        assert_eq!(recipe.navigation_origins, [ORIGIN]);
        assert_eq!(recipe.read_origins, [ORIGIN]);
        assert_eq!(recipe.readiness, CaptureReadiness::OutputsComplete);
        assert_eq!(recipe.poll_interval_millis, 800);
        assert_eq!(recipe.auth_method, AuthMethod::AssistedSession);
        assert_eq!(recipe.session_kind, SessionKind::Composite);
        assert_eq!(recipe.outputs.len(), 2);
        assert!(recipe.outputs.iter().all(|output| output.required));
        let CaptureValueSource::JsonObject { fields } = &recipe.outputs[1].sources[0] else {
            panic!("expected composite JSON capture source");
        };
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "login_info");
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
