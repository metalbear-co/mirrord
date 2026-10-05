mod agent;
mod intproxy;

pub use agent::AgentClient;
pub use intproxy::{IntproxyClient, SessionsManagerConnectInfo};
use mirrord_sessions_manager_protocol::ServiceScope;

use crate::error::SessionsManagerClientError;

/// Rejects a scope whose environment or service is blank or slugifies to nothing.
///
/// Both sessions-manager deployments key scopes by the slugified names, so names that differ only
/// in case or punctuation (`API`, `api`, `a.p.i`) share one scope, and a name with no letters or
/// digits (`"!!!"`) has no scope at all.
pub(crate) fn validate_scope(
    scope: ServiceScope,
) -> Result<ServiceScope, SessionsManagerClientError> {
    validate_scope_part(
        "environment",
        &scope.environment,
        SessionsManagerClientError::MissingConfigEnvironment,
    )?;
    validate_scope_part(
        "service",
        &scope.service,
        SessionsManagerClientError::MissingConfigService,
    )?;

    Ok(scope)
}

fn validate_scope_part(
    field: &'static str,
    value: &str,
    missing: SessionsManagerClientError,
) -> Result<(), SessionsManagerClientError> {
    if value.trim().is_empty() {
        return Err(missing);
    }

    let slug = slug::slugify(value);
    if slug.is_empty() {
        return Err(SessionsManagerClientError::UnsluggableScope {
            field,
            value: value.to_owned(),
        });
    }
    if slug != value {
        tracing::debug!(
            field,
            value,
            slug,
            "sessions-manager scope name is normalized"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use mirrord_sessions_manager_protocol::ServiceScope;

    use super::validate_scope;
    use crate::error::SessionsManagerClientError;

    fn scope(environment: &str, service: &str) -> ServiceScope {
        ServiceScope {
            environment: environment.to_owned(),
            service: service.to_owned(),
        }
    }

    #[test]
    fn rejects_blank_service() {
        assert!(matches!(
            validate_scope(scope("staging", "  ")),
            Err(SessionsManagerClientError::MissingConfigService)
        ));
    }

    #[test]
    fn rejects_punctuation_only_service() {
        assert!(matches!(
            validate_scope(scope("staging", "!!!")),
            Err(SessionsManagerClientError::UnsluggableScope {
                field: "service",
                ..
            })
        ));
    }

    #[test]
    fn rejects_punctuation_only_environment() {
        assert!(matches!(
            validate_scope(scope("///", "api")),
            Err(SessionsManagerClientError::UnsluggableScope {
                field: "environment",
                ..
            })
        ));
    }

    #[test]
    fn accepts_names_that_normalize_to_a_slug() {
        assert!(validate_scope(scope("Staging EU", "API #1")).is_ok());
    }
}
