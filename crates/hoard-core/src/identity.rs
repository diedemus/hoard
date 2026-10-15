use stable_app_core::BuildIdentity;

#[must_use]
pub fn build_identity() -> BuildIdentity {
    let mut identity = BuildIdentity::from_env("hoard", env!("CARGO_PKG_VERSION"));
    identity.candidate_revision = option_env!("PROJECT_CANDIDATE_REVISION")
        .unwrap_or("dev")
        .to_owned();
    identity.framework_version = option_env!("STABLE_BUILD_FRAMEWORK_VERSION")
        .unwrap_or("unknown")
        .to_owned();
    identity.framework_api = option_env!("STABLE_BUILD_FRAMEWORK_API")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    identity.validator_version = option_env!("ADAPTIVE_VALIDATOR_VERSION")
        .unwrap_or("unknown")
        .to_owned();
    identity.validator_api = option_env!("ADAPTIVE_VALIDATOR_API")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    identity
}
