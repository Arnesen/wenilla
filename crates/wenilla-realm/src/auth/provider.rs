//! The seam for external identity providers (Discord in M2, any OIDC issuer later). M1 ships
//! only the trait; `auth::router` mounts `/auth/{provider}/start|callback` for whatever the
//! state's `providers` list holds, which is empty until a provider is configured.

use async_trait::async_trait;

#[derive(Clone, Debug)]
pub struct Identity {
    pub provider: String,
    pub subject: String,
    pub display_name: String,
    pub email: Option<String>,
}

#[async_trait]
pub trait IdentityProvider: Send + Sync {
    /// URL segment and `identities.provider` value, e.g. `discord`.
    fn id(&self) -> &str;
    /// Button label on the login page.
    fn label(&self) -> &str;
    fn authorize_url(
        &self,
        state: &str,
        pkce_challenge: Option<&str>,
        redirect_uri: &str,
    ) -> url::Url;
    async fn callback(
        &self,
        code: &str,
        pkce_verifier: Option<&str>,
        redirect_uri: &str,
    ) -> anyhow::Result<Identity>;
}
