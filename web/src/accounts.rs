use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, StatusCode, Url, header::HeaderValue};
use serde::{Deserialize, Serialize};
use std::{env, time::Duration};
use uuid::Uuid;

pub struct Accounts {
    base: Url,
    key: HeaderValue,
    test_inbox: Option<Url>,
}

#[derive(Deserialize, Serialize)]
pub struct Credentials {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct RecoveryRequest {
    pub email: String,
}

#[derive(Deserialize)]
pub struct NewPassword {
    token: String,
    password: String,
}

#[derive(Deserialize)]
struct User {
    id: Uuid,
    email_confirmed_at: Option<String>,
    #[serde(default)]
    is_anonymous: bool,
}

#[derive(Deserialize)]
struct SignIn {
    access_token: String,
    expires_in: u64,
    refresh_token: Option<String>,
    user: User,
}

pub struct Identity {
    pub user: Uuid,
    pub lifetime: u64,
    pub refresh_token: Option<String>,
}

pub struct Renewal {
    pub token: String,
    pub access_expires: u64,
}

fn loopback_http(url: &Url) -> bool {
    url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
        })
}

impl Accounts {
    pub fn is_loopback_preview(&self) -> bool {
        // URL validation below only permits plain HTTP on loopback.
        self.base.scheme() == "http"
    }

    pub fn test_inbox(&self) -> Option<&str> {
        self.test_inbox.as_ref().map(Url::as_str)
    }

    pub fn configured(public: bool) -> Result<Option<Self>> {
        let test_inbox = env::var_os("EDITROOM_LOCAL_TEST_INBOX_URL");
        match (
            env::var_os("EDITROOM_SUPABASE_URL"),
            env::var_os("EDITROOM_SUPABASE_PUBLISHABLE_KEY"),
        ) {
            (None, None) => {
                ensure!(
                    test_inbox.is_none(),
                    "The test inbox needs a private loopback Supabase preview."
                );
                Ok(None)
            }
            (Some(url), Some(key)) => {
                let url = url.to_str().context("The Supabase URL is invalid.")?;
                let key = key
                    .to_str()
                    .context("The Supabase publishable key is invalid.")?;
                let mut accounts = Self::new(url, key, !public)?;
                if let Some(test_inbox) = test_inbox {
                    ensure!(
                        !public && accounts.is_loopback_preview(),
                        "The test inbox needs a private loopback Supabase preview."
                    );
                    let url = test_inbox
                        .to_str()
                        .and_then(|url| Url::parse(url).ok())
                        .filter(|url| {
                            loopback_http(url)
                                && url.username().is_empty()
                                && url.password().is_none()
                                && url.query().is_none()
                                && url.fragment().is_none()
                        })
                        .context(
                            "Set EDITROOM_LOCAL_TEST_INBOX_URL to an HTTP address on loopback.",
                        )?;
                    accounts.test_inbox = Some(url);
                }
                Ok(Some(accounts))
            }
            _ => anyhow::bail!("Set both the Supabase URL and its publishable key."),
        }
    }

    fn new(url: &str, key: &str, allow_local: bool) -> Result<Self> {
        let mut base = Url::parse(url).context("Set the Supabase project URL.")?;
        let local = allow_local && loopback_http(&base);
        ensure!(
            (base.scheme() == "https" || local)
                && base.host_str().is_some()
                && base.username().is_empty()
                && base.password().is_none()
                && base.path() == "/"
                && base.query().is_none()
                && base.fragment().is_none(),
            "Set an HTTPS Supabase project URL without a path."
        );
        ensure!(
            key.starts_with("sb_publishable_") || legacy_anon_key(key),
            "Use the Supabase publishable or legacy anon key."
        );
        let mut key =
            HeaderValue::from_str(key).context("The Supabase publishable key is invalid.")?;
        key.set_sensitive(true);
        base.set_path("/auth/v1/");
        Ok(Self {
            base,
            key,
            test_inbox: None,
        })
    }

    pub async fn signup(
        &self,
        client: &Client,
        mut credentials: Credentials,
        redirect: &str,
    ) -> Result<bool, StatusCode> {
        credentials.validate()?;
        let response = client
            .post(self.base.join("signup").expect("valid auth endpoint"))
            .query(&[("redirect_to", redirect)])
            .header("apikey", &self.key)
            .json(&credentials)
            .timeout(Duration::from_secs(12))
            .send()
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        check_status(response.status())?;
        let data: serde_json::Value = response.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
        Ok(data["access_token"].as_str().is_none())
    }

    pub async fn signin(
        &self,
        client: &Client,
        mut credentials: Credentials,
    ) -> Result<Identity, StatusCode> {
        credentials.validate()?;
        let response = client
            .post(
                self.base
                    .join("token?grant_type=password")
                    .expect("valid auth endpoint"),
            )
            .header("apikey", &self.key)
            .json(&credentials)
            .timeout(Duration::from_secs(12))
            .send()
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        check_status(response.status())?;
        let result: SignIn = response.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
        result.identity()
    }

    pub async fn refresh(&self, client: &Client, token: &str) -> Result<Identity, StatusCode> {
        let response = client
            .post(
                self.base
                    .join("token?grant_type=refresh_token")
                    .expect("valid auth endpoint"),
            )
            .header("apikey", &self.key)
            .json(&serde_json::json!({"refresh_token":token}))
            .timeout(Duration::from_secs(12))
            .send()
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        check_status(response.status())?;
        let result: SignIn = response.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
        result.identity()
    }

    pub async fn send_recovery(
        &self,
        client: &Client,
        request: RecoveryRequest,
        redirect: &str,
    ) -> Result<(), StatusCode> {
        let email = request.email.trim();
        if !valid_email(email) {
            return Err(StatusCode::BAD_REQUEST);
        }
        let response = client
            .post(self.base.join("recover").expect("valid auth endpoint"))
            .query(&[("redirect_to", redirect)])
            .header("apikey", &self.key)
            .json(&serde_json::json!({"email":email}))
            .timeout(Duration::from_secs(12))
            .send()
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => Ok(()),
            StatusCode::TOO_MANY_REQUESTS => Err(StatusCode::TOO_MANY_REQUESTS),
            _ => Err(StatusCode::BAD_GATEWAY),
        }
    }

    pub async fn reset_password(
        &self,
        client: &Client,
        request: NewPassword,
    ) -> Result<Uuid, StatusCode> {
        if !valid_password(&request.password)
            || request.token.is_empty()
            || request.token.len() > 8192
            || !request
                .token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        let response = client
            .put(self.base.join("user").expect("valid auth endpoint"))
            .header("apikey", &self.key)
            .bearer_auth(&request.token)
            .json(&serde_json::json!({"password":request.password}))
            .timeout(Duration::from_secs(12))
            .send()
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        match response.status() {
            StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                return Err(StatusCode::UNPROCESSABLE_ENTITY);
            }
            status => check_status(status)?,
        }
        let user: User = response.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
        if user.id.is_nil() || user.is_anonymous {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(user.id)
    }
}

impl SignIn {
    fn identity(self) -> Result<Identity, StatusCode> {
        if self.user.id.is_nil()
            || self
                .user
                .email_confirmed_at
                .as_deref()
                .is_none_or(str::is_empty)
            || self.user.is_anonymous
            || self.access_token.is_empty()
            || self.expires_in == 0
        {
            return Err(StatusCode::UNAUTHORIZED);
        }
        Ok(Identity {
            user: self.user.id,
            lifetime: self.expires_in,
            refresh_token: self.refresh_token.filter(|token| !token.is_empty()),
        })
    }
}

impl Credentials {
    fn validate(&mut self) -> Result<(), StatusCode> {
        self.email = self.email.trim().to_owned();
        if !valid_email(&self.email) || !valid_password(&self.password) {
            return Err(StatusCode::BAD_REQUEST);
        }
        Ok(())
    }
}

fn valid_email(email: &str) -> bool {
    email.len() <= 254 && email.contains('@')
}

fn valid_password(password: &str) -> bool {
    !password.is_empty() && password.len() <= 1024
}

fn check_status(status: StatusCode) -> Result<(), StatusCode> {
    match status {
        StatusCode::TOO_MANY_REQUESTS => Err(status),
        status if status.is_success() => Ok(()),
        status if status.is_client_error() => Err(StatusCode::UNAUTHORIZED),
        _ => Err(StatusCode::BAD_GATEWAY),
    }
}

fn legacy_anon_key(key: &str) -> bool {
    let mut parts = key.split('.');
    parts.next().is_some()
        && parts
            .next()
            .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
            .and_then(|payload| serde_json::from_slice::<serde_json::Value>(&payload).ok())
            .is_some_and(|value| value["role"] == "anon")
        && parts.next().is_some()
        && parts.next().is_none()
}
