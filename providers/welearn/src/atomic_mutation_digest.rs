use asterism_provider_api::{ProviderError, ProviderErrorKind, ProviderResult};
use reqwest::Url;
use sha2::{Digest, Sha256};

use crate::{
    WellearnAtomicCompletionProfile, WellearnAtomicDurationCompletionDocuments,
    WellearnAtomicDurationCompletionPlan, WellearnAtomicMutationKind, WellearnInventoryDocument,
    duration_report::WellearnDurationMutationKind,
    resource_execution::WellearnResourceMutationKind,
};

const REQUEST_DOMAIN: &[u8] = b"asterism.welearn.atomic-mutation-request.v1\0";
const RESPONSE_DOMAIN: &[u8] = b"asterism.welearn.atomic-mutation-response.v1\0";
const RESOURCE_REQUEST_DOMAIN: &[u8] = b"asterism.welearn.resource-mutation-request.v1\0";
const RESOURCE_RESPONSE_DOMAIN: &[u8] = b"asterism.welearn.resource-mutation-response.v1\0";
const DURATION_REQUEST_DOMAIN: &[u8] = b"asterism.welearn.duration-mutation-request.v1\0";
const DURATION_RESPONSE_DOMAIN: &[u8] = b"asterism.welearn.duration-mutation-response.v1\0";
const COMPLETION_OBSERVATION_DOMAIN: &[u8] = b"asterism.welearn.atomic-completion-observation.v1\0";
const MAX_MUTATION_ORDINAL: u32 = 100_000;
const MAX_MUTATION_FIELDS: usize = 16;
const MAX_URL_BYTES: usize = 4_096;
const MAX_FIELD_NAME_BYTES: usize = 64;
const MAX_FIELD_VALUE_BYTES: usize = 1_024 * 1_024;

/// Hashes one exact semantic `WELearn` mutation request without accepting a
/// Cookie input. Field order remains part of the request identity.
pub(crate) fn atomic_mutation_request_digest(
    kind: WellearnAtomicMutationKind,
    ordinal: u32,
    endpoint: &Url,
    referer: &Url,
    fields: &[(&str, &str)],
) -> ProviderResult<[u8; 32]> {
    mutation_request_digest(
        REQUEST_DOMAIN,
        kind.as_str(),
        ordinal,
        endpoint,
        referer,
        fields,
    )
}

/// Hashes one exact singleton resource mutation request without accepting a
/// Cookie input.
pub(crate) fn resource_mutation_request_digest(
    kind: WellearnResourceMutationKind,
    ordinal: u32,
    endpoint: &Url,
    referer: &Url,
    fields: &[(&str, &str)],
) -> ProviderResult<[u8; 32]> {
    mutation_request_digest(
        RESOURCE_REQUEST_DOMAIN,
        kind.as_str(),
        ordinal,
        endpoint,
        referer,
        fields,
    )
}

/// Hashes one exact singleton duration mutation request in a domain distinct
/// from atomic duration-completion and singleton `ResourceExecution`.
pub(crate) fn duration_mutation_request_digest(
    kind: WellearnDurationMutationKind,
    ordinal: u32,
    endpoint: &Url,
    referer: &Url,
    fields: &[(&str, &str)],
) -> ProviderResult<[u8; 32]> {
    mutation_request_digest(
        DURATION_REQUEST_DOMAIN,
        kind.as_str(),
        ordinal,
        endpoint,
        referer,
        fields,
    )
}

fn mutation_request_digest(
    domain: &[u8],
    operation_type: &str,
    ordinal: u32,
    endpoint: &Url,
    referer: &Url,
    fields: &[(&str, &str)],
) -> ProviderResult<[u8; 32]> {
    validate_request_identity(ordinal, endpoint, referer, fields)?;
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(ordinal.to_be_bytes());
    hash_component(&mut hash, operation_type.as_bytes())?;
    hash_component(&mut hash, endpoint.as_str().as_bytes())?;
    hash_component(&mut hash, referer.as_str().as_bytes())?;
    hash.update(
        u32::try_from(fields.len())
            .map_err(|_| invalid_digest_input())?
            .to_be_bytes(),
    );
    for (name, value) in fields {
        hash_component(&mut hash, name.as_bytes())?;
        hash_component(&mut hash, value.as_bytes())?;
    }
    Ok(hash.finalize().into())
}

/// Hashes the exact bounded response text returned by the native reader.
pub(crate) fn atomic_mutation_response_digest(
    document: &WellearnInventoryDocument,
) -> ProviderResult<[u8; 32]> {
    mutation_response_digest(RESPONSE_DOMAIN, document)
}

fn mutation_response_digest(
    domain: &[u8],
    document: &WellearnInventoryDocument,
) -> ProviderResult<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash_component(&mut hash, document.as_str().as_bytes())?;
    Ok(hash.finalize().into())
}

/// Hashes the exact bounded singleton resource mutation response text.
pub(crate) fn resource_mutation_response_digest(
    document: &WellearnInventoryDocument,
) -> ProviderResult<[u8; 32]> {
    mutation_response_digest(RESOURCE_RESPONSE_DOMAIN, document)
}

/// Hashes the exact bounded singleton duration response text.
pub(crate) fn duration_mutation_response_digest(
    document: &WellearnInventoryDocument,
) -> ProviderResult<[u8; 32]> {
    mutation_response_digest(DURATION_RESPONSE_DOMAIN, document)
}

/// Hashes only the fresh CMI evidence used by the exact compound verifier and
/// binds it to the frozen goal and final completion-save ordinal.
pub(crate) fn atomic_completion_observation_digest(
    plan: WellearnAtomicDurationCompletionPlan,
    documents: &WellearnAtomicDurationCompletionDocuments,
    final_save_ordinal: u32,
) -> ProviderResult<[u8; 32]> {
    plan.validate()?;
    documents.validate_for_plan(plan)?;
    if final_save_ordinal != documents.final_save_ordinal(plan)? {
        return Err(invalid_digest_input());
    }
    let profile = match plan.profile() {
        WellearnAtomicCompletionProfile::FanyuchangFreshSetSave100 => {
            b"fanyuchang_fresh_set_save_100".as_slice()
        }
        WellearnAtomicCompletionProfile::AutoZeroTimeSaveOnly0 => {
            b"auto_zero_time_save_only_0".as_slice()
        }
    };
    let mut hash = Sha256::new();
    hash.update(COMPLETION_OBSERVATION_DOMAIN);
    hash.update(final_save_ordinal.to_be_bytes());
    hash.update(plan.target_seconds().to_be_bytes());
    hash_component(&mut hash, profile)?;
    match documents.after_duration() {
        Some(document) => {
            hash.update([1]);
            hash_component(&mut hash, document.as_str().as_bytes())?;
        }
        None => hash.update([0]),
    }
    hash_component(&mut hash, documents.after_completion().as_str().as_bytes())?;
    Ok(hash.finalize().into())
}

fn validate_request_identity(
    ordinal: u32,
    endpoint: &Url,
    referer: &Url,
    fields: &[(&str, &str)],
) -> ProviderResult<()> {
    if !(1..=MAX_MUTATION_ORDINAL).contains(&ordinal)
        || fields.is_empty()
        || fields.len() > MAX_MUTATION_FIELDS
        || !valid_https_url(endpoint)
        || !valid_https_url(referer)
    {
        return Err(invalid_digest_input());
    }
    let mut total_field_bytes = 0_usize;
    for (name, value) in fields {
        total_field_bytes = total_field_bytes
            .checked_add(name.len())
            .and_then(|total| total.checked_add(value.len()))
            .ok_or_else(invalid_digest_input)?;
        if name.is_empty()
            || name.len() > MAX_FIELD_NAME_BYTES
            || !name.is_ascii()
            || name.chars().any(char::is_control)
            || name.eq_ignore_ascii_case("cookie")
            || value.len() > MAX_FIELD_VALUE_BYTES
            || total_field_bytes > MAX_FIELD_VALUE_BYTES
        {
            return Err(invalid_digest_input());
        }
    }
    Ok(())
}

fn valid_https_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.as_str().len() <= MAX_URL_BYTES
        && url.fragment().is_none()
}

fn hash_component(hash: &mut Sha256, value: &[u8]) -> ProviderResult<()> {
    hash.update(
        u64::try_from(value.len())
            .map_err(|_| invalid_digest_input())?
            .to_be_bytes(),
    );
    hash.update(value);
    Ok(())
}

fn invalid_digest_input() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::Internal,
        "WELearn atomic mutation digest input is invalid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use asterism_provider_api::ExecutionMutationIssue;

    #[test]
    fn request_digest_is_stable_and_binds_every_ordered_component() {
        let endpoint = url("https://welearn.sflep.com/Ajax/SCO.aspx?uid=7001");
        let referer = url("https://welearn.sflep.com/student/StudyCourse.aspx");
        let fields = [("action", "startsco160928"), ("cid", "1001")];
        let baseline = atomic_mutation_request_digest(
            WellearnAtomicMutationKind::Start,
            1,
            &endpoint,
            &referer,
            &fields,
        )
        .unwrap();
        assert_eq!(
            baseline,
            atomic_mutation_request_digest(
                WellearnAtomicMutationKind::Start,
                1,
                &endpoint,
                &referer,
                &fields,
            )
            .unwrap()
        );

        let variants = [
            atomic_mutation_request_digest(
                WellearnAtomicMutationKind::CounterKeep,
                1,
                &endpoint,
                &referer,
                &fields,
            )
            .unwrap(),
            atomic_mutation_request_digest(
                WellearnAtomicMutationKind::Start,
                2,
                &endpoint,
                &referer,
                &fields,
            )
            .unwrap(),
            atomic_mutation_request_digest(
                WellearnAtomicMutationKind::Start,
                1,
                &url("https://welearn.sflep.com/Ajax/SCO.aspx"),
                &referer,
                &fields,
            )
            .unwrap(),
            atomic_mutation_request_digest(
                WellearnAtomicMutationKind::Start,
                1,
                &endpoint,
                &url("https://welearn.sflep.com/student/StudyCourse.aspx?cid=1001"),
                &fields,
            )
            .unwrap(),
            atomic_mutation_request_digest(
                WellearnAtomicMutationKind::Start,
                1,
                &endpoint,
                &referer,
                &[("cid", "1001"), ("action", "startsco160928")],
            )
            .unwrap(),
        ];
        assert!(variants.into_iter().all(|digest| digest != baseline));
    }

    #[test]
    fn length_prefixes_prevent_field_boundary_aliases() {
        let endpoint = url("https://welearn.sflep.com/Ajax/SCO.aspx");
        let referer = url("https://welearn.sflep.com/student/StudyCourse.aspx");
        let left = atomic_mutation_request_digest(
            WellearnAtomicMutationKind::Set,
            2,
            &endpoint,
            &referer,
            &[("ab", "c")],
        )
        .unwrap();
        let right = atomic_mutation_request_digest(
            WellearnAtomicMutationKind::Set,
            2,
            &endpoint,
            &referer,
            &[("a", "bc")],
        )
        .unwrap();
        assert_ne!(left, right);
    }

    #[test]
    fn cookie_cannot_enter_the_request_digest_contract() {
        let endpoint = url("https://welearn.sflep.com/Ajax/SCO.aspx");
        let referer = url("https://welearn.sflep.com/student/StudyCourse.aspx");
        assert!(
            atomic_mutation_request_digest(
                WellearnAtomicMutationKind::Start,
                1,
                &endpoint,
                &referer,
                &[("Cookie", "secret-session")],
            )
            .is_err()
        );

        let digest = atomic_mutation_request_digest(
            WellearnAtomicMutationKind::Start,
            1,
            &endpoint,
            &referer,
            &[("action", "startsco160928")],
        )
        .unwrap();
        let issue =
            ExecutionMutationIssue::new(1, WellearnAtomicMutationKind::Start.as_str(), digest)
                .unwrap();
        let debug = format!("{issue:?}");
        assert!(!debug.contains(endpoint.as_str()));
        assert!(!debug.contains("startsco160928"));
        assert!(!debug.contains("secret-session"));
        assert!(debug.contains("[HASHED]"));
    }

    #[test]
    fn response_digest_is_domain_separated_and_binds_bounded_original_text() {
        let first = WellearnInventoryDocument::try_new(r#"{"ret":0}"#.to_owned()).unwrap();
        let second = WellearnInventoryDocument::try_new(r#"{"ret":1}"#.to_owned()).unwrap();
        let first_digest = atomic_mutation_response_digest(&first).unwrap();
        assert_eq!(
            first_digest,
            atomic_mutation_response_digest(&first).unwrap()
        );
        assert_ne!(
            first_digest,
            atomic_mutation_response_digest(&second).unwrap()
        );

        let endpoint = url("https://welearn.sflep.com/Ajax/SCO.aspx");
        let referer = url("https://welearn.sflep.com/student/StudyCourse.aspx");
        let request_digest = atomic_mutation_request_digest(
            WellearnAtomicMutationKind::Start,
            1,
            &endpoint,
            &referer,
            &[("action", "startsco160928")],
        )
        .unwrap();
        assert_ne!(first_digest, request_digest);
        assert_eq!(
            format!("{first:?}"),
            "WellearnInventoryDocument([REDACTED])"
        );
    }

    #[test]
    fn resource_digests_are_domain_separated_and_bind_the_exact_exchange() {
        let endpoint = url("https://welearn.sflep.com/Ajax/SCO.aspx?uid=7001");
        let referer = url("https://welearn.sflep.com/student/StudyCourse.aspx");
        let fields = [("action", "startsco160928"), ("cid", "1001")];
        let resource = resource_mutation_request_digest(
            WellearnResourceMutationKind::Start,
            1,
            &endpoint,
            &referer,
            &fields,
        )
        .unwrap();
        let atomic = atomic_mutation_request_digest(
            WellearnAtomicMutationKind::Start,
            1,
            &endpoint,
            &referer,
            &fields,
        )
        .unwrap();
        assert_ne!(resource, atomic);
        assert_ne!(
            resource,
            resource_mutation_request_digest(
                WellearnResourceMutationKind::Set,
                1,
                &endpoint,
                &referer,
                &fields,
            )
            .unwrap()
        );

        let document = WellearnInventoryDocument::try_new(r#"{"ret":0}"#.to_owned()).unwrap();
        assert_ne!(
            resource_mutation_response_digest(&document).unwrap(),
            atomic_mutation_response_digest(&document).unwrap()
        );
    }

    #[test]
    fn duration_digests_are_separate_from_atomic_and_resource_domains() {
        let endpoint = url("https://welearn.sflep.com/Ajax/SCO.aspx?uid=7001");
        let referer = url("https://welearn.sflep.com/student/StudyCourse.aspx");
        let fields = [
            ("action", "keepsco_with_getticket_with_updatecmitime"),
            ("cid", "1001"),
        ];
        let duration = duration_mutation_request_digest(
            WellearnDurationMutationKind::CounterKeep,
            2,
            &endpoint,
            &referer,
            &fields,
        )
        .unwrap();
        let atomic = atomic_mutation_request_digest(
            WellearnAtomicMutationKind::CounterKeep,
            2,
            &endpoint,
            &referer,
            &fields,
        )
        .unwrap();
        assert_ne!(duration, atomic);

        let document = WellearnInventoryDocument::try_new(r#"{"ret":0}"#.to_owned()).unwrap();
        assert_ne!(
            duration_mutation_response_digest(&document).unwrap(),
            atomic_mutation_response_digest(&document).unwrap()
        );
        assert_ne!(
            duration_mutation_response_digest(&document).unwrap(),
            resource_mutation_response_digest(&document).unwrap()
        );
    }

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }
}
