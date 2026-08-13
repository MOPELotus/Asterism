use std::{fmt, fmt::Write as _};

use asterism_provider_api::{
    ExternalOauthAuthorization, ExternalOauthCallbackBinding, ProviderError, ProviderErrorKind,
    ProviderResult,
};
use asterism_secrets::SecretString;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use reqwest::Url;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::oauth_exchange::CidarenOauthCode;

const WECHAT_AUTHORIZE_URL: &str = "https://open.weixin.qq.com/connect/oauth2/authorize";
const WECHAT_APP_ID: &str = "wx2a694105a6abbe6d";
const WECHAT_SCOPE: &str = "snsapi_userinfo";
const RESERVED_OFFICIAL_MARKER: &str = "2";
const MAX_CALLBACK_URL_BYTES: usize = 16 * 1_024;
const MAX_CALLBACK_VALUE_BYTES: usize = 512;
const OAUTH_STATE_BYTES: usize = 32;
const AUTHORIZE_MARKER_BYTES: usize = 16;

/// Hash-only binding which Core may persist with one owner/AuthSession-bound
/// pending login. It contains neither OAuth callback code nor raw state or
/// authorize-marker material.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct CidarenOauthCallbackBinding {
    state_digest: [u8; 32],
    authorize_marker_digest: [u8; 32],
}

impl CidarenOauthCallbackBinding {
    /// Reconstructs the Provider binding from Core's two fixed-width durable
    /// digest fields after an owner/AuthSession-bound pending-login reload.
    pub(crate) const fn from_digests(
        state_digest: [u8; 32],
        authorize_marker_digest: [u8; 32],
    ) -> Self {
        Self {
            state_digest,
            authorize_marker_digest,
        }
    }

    #[cfg(test)]
    /// Returns the domain-separated state digest for test reconstruction.
    const fn state_digest(&self) -> [u8; 32] {
        self.state_digest
    }

    #[cfg(test)]
    /// Returns the independently domain-separated marker digest for tests.
    const fn authorize_marker_digest(&self) -> [u8; 32] {
        self.authorize_marker_digest
    }

    fn matches(&self, state: &str, authorize_marker: &str) -> bool {
        constant_time_equal(&self.state_digest, &oauth_state_digest(state))
            & constant_time_equal(
                &self.authorize_marker_digest,
                &authorize_marker_digest(authorize_marker),
            )
    }

    pub(crate) fn from_external(binding: ExternalOauthCallbackBinding) -> ProviderResult<Self> {
        if !binding.validate() {
            return Err(invalid_callback());
        }
        Ok(Self::from_digests(
            binding.state_digest(),
            binding.provider_context_digest(),
        ))
    }

    const fn into_external(self) -> ExternalOauthCallbackBinding {
        ExternalOauthCallbackBinding::from_digests(self.state_digest, self.authorize_marker_digest)
    }
}

impl fmt::Debug for CidarenOauthCallbackBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CidarenOauthCallbackBinding([HASHED])")
    }
}

/// One fresh `WeChat` OAuth authorization URL plus its hash-only callback
/// binding. The URL itself contains short-lived state and is therefore
/// zeroized and redacted even though it is intentionally shown to the user.
pub(crate) struct CidarenOauthAuthorization {
    oauth_url: SecretString,
    callback_binding: CidarenOauthCallbackBinding,
}

impl CidarenOauthAuthorization {
    /// Generates independent CSPRNG state and authorize marker values, builds
    /// the audited `WeChat` OAuth URL and retains only their digests outside the
    /// returned short-lived URL.
    ///
    /// # Errors
    ///
    /// Returns Internal if operating-system randomness or a frozen static URL
    /// unexpectedly cannot be used.
    pub(crate) fn generate() -> ProviderResult<Self> {
        let mut state_random = Zeroizing::new([0_u8; OAUTH_STATE_BYTES]);
        OsRng
            .try_fill_bytes(state_random.as_mut())
            .map_err(|_| internal("Cidaren OAuth state generation failed"))?;
        let mut marker_random = Zeroizing::new([0_u8; AUTHORIZE_MARKER_BYTES]);
        OsRng
            .try_fill_bytes(marker_random.as_mut())
            .map_err(|_| internal("Cidaren OAuth authorize-marker generation failed"))?;
        Self::from_random_bytes(&state_random, &marker_random)
    }

    fn from_random_bytes(
        state_random: &[u8; OAUTH_STATE_BYTES],
        marker_random: &[u8; AUTHORIZE_MARKER_BYTES],
    ) -> ProviderResult<Self> {
        let state = Zeroizing::new(URL_SAFE_NO_PAD.encode(state_random));
        let mut authorize_marker = Zeroizing::new(String::from("x"));
        for byte in marker_random {
            write!(authorize_marker, "{byte:02x}")
                .map_err(|_| internal("Cidaren OAuth authorize-marker encoding failed"))?;
        }
        if authorize_marker.as_str() == RESERVED_OFFICIAL_MARKER {
            return Err(internal(
                "Cidaren OAuth generated a reserved authorize marker",
            ));
        }
        let callback_binding = CidarenOauthCallbackBinding {
            state_digest: oauth_state_digest(&state),
            authorize_marker_digest: authorize_marker_digest(&authorize_marker),
        };
        let oauth_url = build_oauth_url(&state, &authorize_marker)?;
        Ok(Self {
            oauth_url: SecretString::new(oauth_url),
            callback_binding,
        })
    }

    /// Exposes the authorization URL only to the authenticated product surface
    /// which displays, copies or renders it as a QR code.
    fn expose_oauth_url(&self) -> &str {
        self.oauth_url.expose_secret()
    }

    #[cfg(test)]
    /// Returns the Provider-private callback binding for parser tests.
    const fn callback_binding(&self) -> CidarenOauthCallbackBinding {
        self.callback_binding
    }

    pub(crate) fn into_external(self) -> ProviderResult<ExternalOauthAuthorization> {
        let authorization = ExternalOauthAuthorization {
            authorization_url: self.expose_oauth_url().to_owned(),
            callback_binding: self.callback_binding.into_external(),
        };
        if authorization.validate() {
            Ok(authorization)
        } else {
            Err(internal(
                "Cidaren OAuth authorization violates the shared contract",
            ))
        }
    }
}

impl fmt::Debug for CidarenOauthAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CidarenOauthAuthorization")
            .field("oauth_url", &"[REDACTED]")
            .field("callback_binding", &self.callback_binding)
            .finish()
    }
}

/// Strictly validates one manually returned Cidaren callback URL against a
/// hash-only pending-login binding and extracts its single-use OAuth code.
/// Query parameters and the audited SPA `fragment?...` form are accepted, but
/// duplicate security parameters are always rejected rather than merged.
///
/// # Errors
///
/// Returns Authentication for state/marker mismatch and `InvalidResponse` for
/// an unsafe URL, wrong origin/path, duplicate values or invalid code.
pub(crate) fn parse_oauth_callback(
    callback_url: impl Into<String>,
    expected: &CidarenOauthCallbackBinding,
) -> ProviderResult<CidarenOauthCode> {
    let callback_url = Zeroizing::new(callback_url.into());
    if callback_url.is_empty()
        || callback_url.len() > MAX_CALLBACK_URL_BYTES
        || callback_url.trim() != callback_url.as_str()
        || callback_url.chars().any(char::is_control)
    {
        return Err(invalid_callback());
    }
    let parsed = Url::parse(&callback_url).map_err(|_| invalid_callback())?;
    validate_callback_origin(callback_url.as_str(), &parsed)?;
    let mut parameters = CallbackParameters::default();
    parameters.read_pairs(parsed.query_pairs())?;
    if let Some(fragment_query) = parsed
        .fragment()
        .and_then(|fragment| fragment.split_once('?'))
    {
        let mut fragment_url = Url::parse("https://fragment.invalid/")
            .map_err(|_| internal("Cidaren callback parser initialization failed"))?;
        fragment_url.set_query(Some(fragment_query.1));
        parameters.read_pairs(fragment_url.query_pairs())?;
    }
    let mut state = parameters.state.take().ok_or_else(invalid_callback)?;
    let mut authorize_marker = parameters
        .authorize_marker
        .take()
        .ok_or_else(invalid_callback)?;
    if !valid_callback_value(&state)
        || !valid_callback_value(&authorize_marker)
        || authorize_marker.as_str() == RESERVED_OFFICIAL_MARKER
    {
        return Err(invalid_callback());
    }
    if !expected.matches(&state, &authorize_marker) {
        state.zeroize();
        authorize_marker.zeroize();
        return Err(ProviderError::new(
            ProviderErrorKind::Authentication,
            "Cidaren OAuth callback does not match the pending login",
        ));
    }
    state.zeroize();
    authorize_marker.zeroize();
    let code = parameters.code.take().ok_or_else(invalid_callback)?;
    CidarenOauthCode::try_new(code.to_string()).map_err(|_| invalid_callback())
}

fn build_oauth_url(state: &str, authorize_marker: &str) -> ProviderResult<String> {
    if !valid_callback_value(state)
        || !valid_callback_value(authorize_marker)
        || authorize_marker == RESERVED_OFFICIAL_MARKER
    {
        return Err(internal("Cidaren OAuth generated invalid callback binding"));
    }
    Ok(format!(
        "{WECHAT_AUTHORIZE_URL}?appid={WECHAT_APP_ID}&redirect_uri=https%3A%2F%2Fapp.vocabgo.com%2Fstudent%2F%3Fauthorize%3D{authorize_marker}&response_type=code&scope={WECHAT_SCOPE}&state={state}#wechat_redirect"
    ))
}

fn validate_callback_origin(raw_url: &str, url: &Url) -> ProviderResult<()> {
    const EXACT_ORIGIN: &str = "https://app.vocabgo.com";
    let raw_path = raw_url
        .strip_prefix(EXACT_ORIGIN)
        .map(|remainder| {
            let path_end = remainder.find(['?', '#']).unwrap_or(remainder.len());
            &remainder[..path_end]
        })
        .filter(|path| matches!(*path, "/student" | "/student/"));
    if raw_path.is_none()
        || url.scheme() != "https"
        || url.host_str() != Some("app.vocabgo.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "/student" | "/student/")
    {
        Err(invalid_callback())
    } else {
        Ok(())
    }
}

#[derive(Default)]
struct CallbackParameters {
    code: Option<Zeroizing<String>>,
    state: Option<Zeroizing<String>>,
    authorize_marker: Option<Zeroizing<String>>,
}

impl CallbackParameters {
    fn read_pairs<'a>(
        &mut self,
        pairs: impl Iterator<Item = (std::borrow::Cow<'a, str>, std::borrow::Cow<'a, str>)>,
    ) -> ProviderResult<()> {
        for (name, value) in pairs {
            let target = match name.as_ref() {
                "code" => &mut self.code,
                "state" => &mut self.state,
                "authorize" => &mut self.authorize_marker,
                _ => continue,
            };
            if target.is_some() || value.is_empty() || value.len() > MAX_CALLBACK_VALUE_BYTES {
                return Err(invalid_callback());
            }
            *target = Some(Zeroizing::new(value.into_owned()));
        }
        Ok(())
    }
}

fn valid_callback_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_CALLBACK_VALUE_BYTES
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
}

fn oauth_state_digest(value: &str) -> [u8; 32] {
    digest(b"asterism:cidaren:oauth-state:v1\0", value)
}

fn authorize_marker_digest(value: &str) -> [u8; 32] {
    digest(b"asterism:cidaren:authorize-marker:v1\0", value)
}

fn digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(value.as_bytes());
    hasher.finalize().into()
}

fn constant_time_equal(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn invalid_callback() -> ProviderError {
    ProviderError::new(
        ProviderErrorKind::InvalidResponse,
        "Cidaren OAuth callback URL is unsafe, malformed or ambiguous",
    )
}

fn internal(message: &'static str) -> ProviderError {
    ProviderError::new(ProviderErrorKind::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_url_is_fresh_exact_and_redacted() {
        let first = CidarenOauthAuthorization::generate().unwrap();
        let second = CidarenOauthAuthorization::generate().unwrap();
        assert_ne!(first.callback_binding(), second.callback_binding());
        assert_ne!(first.expose_oauth_url(), second.expose_oauth_url());

        let oauth = Url::parse(first.expose_oauth_url()).unwrap();
        assert_eq!(oauth.scheme(), "https");
        assert_eq!(oauth.host_str(), Some("open.weixin.qq.com"));
        assert_eq!(oauth.path(), "/connect/oauth2/authorize");
        assert_eq!(oauth.fragment(), Some("wechat_redirect"));
        let query = oauth
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(query.get("appid").map(AsRef::as_ref), Some(WECHAT_APP_ID));
        assert_eq!(query.get("response_type").map(AsRef::as_ref), Some("code"));
        assert_eq!(query.get("scope").map(AsRef::as_ref), Some(WECHAT_SCOPE));
        let state = query.get("state").unwrap();
        assert!(valid_callback_value(state));
        let callback = Url::parse(query.get("redirect_uri").unwrap()).unwrap();
        let callback_query = callback
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        let marker = callback_query.get("authorize").unwrap();
        assert_ne!(marker.as_ref(), RESERVED_OFFICIAL_MARKER);
        assert!(valid_callback_value(marker));
        assert!(
            first.callback_binding().matches(state, marker),
            "hash-only binding must match the URL's raw values"
        );
        let restored = CidarenOauthCallbackBinding::from_digests(
            first.callback_binding().state_digest(),
            first.callback_binding().authorize_marker_digest(),
        );
        assert_eq!(restored, first.callback_binding());
        let deterministic = deterministic_authorization();
        let deterministic_binding = deterministic.callback_binding();
        let shared = deterministic.into_external().unwrap();
        assert!(shared.validate());
        assert_eq!(
            shared.callback_binding.state_digest(),
            deterministic_binding.state_digest()
        );
        assert_eq!(
            shared.callback_binding.provider_context_digest(),
            deterministic_binding.authorize_marker_digest()
        );
        assert_eq!(
            CidarenOauthCallbackBinding::from_external(shared.callback_binding).unwrap(),
            deterministic_binding
        );
        let debug = format!("{first:?}");
        assert!(!debug.contains(state.as_ref()));
        assert!(!debug.contains(marker.as_ref()));
        assert!(!debug.contains("open.weixin.qq.com"));

        assert_eq!(
            deterministic_authorization().expose_oauth_url(),
            "https://open.weixin.qq.com/connect/oauth2/authorize?appid=wx2a694105a6abbe6d&redirect_uri=https%3A%2F%2Fapp.vocabgo.com%2Fstudent%2F%3Fauthorize%3Dx000102030405060708090a0b0c0d0e0f&response_type=code&scope=snsapi_userinfo&state=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8#wechat_redirect"
        );
    }

    #[test]
    fn callback_parser_accepts_exact_query_and_spa_fragment_forms() {
        let authorization = deterministic_authorization();
        for callback in [
            "https://app.vocabgo.com/student/?authorize=x000102030405060708090a0b0c0d0e0f&code=synthetic-code&state=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
            "https://app.vocabgo.com/student/#/home?authorize=x000102030405060708090a0b0c0d0e0f&code=synthetic-code&state=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
        ] {
            let code = parse_oauth_callback(callback, &authorization.callback_binding()).unwrap();
            assert_eq!(code.expose_secret(), "synthetic-code");
            assert!(!format!("{code:?}").contains("synthetic-code"));
        }
    }

    #[test]
    fn callback_parser_rejects_origin_binding_and_parameter_ambiguity() {
        let binding = deterministic_authorization().callback_binding();
        let valid_query = "authorize=x000102030405060708090a0b0c0d0e0f&code=synthetic-code&state=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
        for callback in [
            format!("http://app.vocabgo.com/student/?{valid_query}"),
            format!("https://evil.example/student/?{valid_query}"),
            format!("https://app.vocabgo.com.evil.example/student/?{valid_query}"),
            format!("https://user@app.vocabgo.com/student/?{valid_query}"),
            format!("https://app.vocabgo.com:443/student/?{valid_query}"),
            format!("https://app.vocabgo.com/other/?{valid_query}"),
            format!("https://app.vocabgo.com/student/../student/?{valid_query}"),
            format!("https://app.vocabgo.com/%73tudent/?{valid_query}"),
            format!("https://app.vocabgo.com/student/?{valid_query}&code=duplicate"),
            format!("https://app.vocabgo.com/student/?{valid_query}#/x?state=duplicate"),
            format!("https://app.vocabgo.com/student/?{valid_query}&authorize=2"),
        ] {
            assert!(parse_oauth_callback(callback, &binding).is_err());
        }

        for callback in [
            "https://app.vocabgo.com/student/?authorize=x000102030405060708090a0b0c0d0e0f&state=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
            "https://app.vocabgo.com/student/?authorize=x000102030405060708090a0b0c0d0e0f&code=&state=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
            "https://app.vocabgo.com/student/?authorize=x000102030405060708090a0b0c0d0e0f&code=bad%2Fcode&state=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
        ] {
            assert_eq!(
                parse_oauth_callback(callback, &binding).unwrap_err().kind,
                ProviderErrorKind::InvalidResponse
            );
        }
        assert_eq!(
            parse_oauth_callback(
                format!(
                    "https://app.vocabgo.com/student/?{valid_query}{}",
                    "x".repeat(MAX_CALLBACK_URL_BYTES)
                ),
                &binding,
            )
            .unwrap_err()
            .kind,
            ProviderErrorKind::InvalidResponse
        );

        for callback in [
            "https://app.vocabgo.com/student/?authorize=x000102030405060708090a0b0c0d0e0e&code=synthetic-code&state=AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
            "https://app.vocabgo.com/student/?authorize=x000102030405060708090a0b0c0d0e0f&code=synthetic-code&state=wrong-state",
        ] {
            assert_eq!(
                parse_oauth_callback(callback, &binding).unwrap_err().kind,
                ProviderErrorKind::Authentication
            );
        }
    }

    fn deterministic_authorization() -> CidarenOauthAuthorization {
        CidarenOauthAuthorization::from_random_bytes(
            &std::array::from_fn(|index| u8::try_from(index).unwrap()),
            &std::array::from_fn(|index| u8::try_from(index).unwrap()),
        )
        .unwrap()
    }
}
