use super::*;
use crate::accounts::{Accounts, Credentials, NewPassword, RecoveryRequest, Renewal};
use crate::billing::Billing;
use axum::body::Bytes;

struct Gateway {
    auth: Accounts,
    billing: Option<Billing>,
    local: Shared,
    public_routes: Router,
    root: PathBuf,
    initial_credits: u32,
    workspaces: tokio::sync::Mutex<HashMap<Uuid, Workspace>>,
}

#[derive(Clone)]
struct Workspace {
    app: Shared,
    router: Router,
    checkout_attempts: Arc<Mutex<Vec<u64>>>,
}

pub(super) async fn routes(
    local: Shared,
    auth: Accounts,
    billing: Option<Billing>,
    root: PathBuf,
    initial_credits: u32,
) -> Result<Router> {
    let gateway = Arc::new(Gateway {
        auth,
        billing,
        public_routes: workspace_routes(local.clone()),
        local,
        root,
        initial_credits,
        workspaces: tokio::sync::Mutex::default(),
    });
    gateway.restore().await?;
    Ok(Router::new()
        .route("/api/account/config", get(account_config))
        .route(
            "/api/account/signup",
            post(signup).layer(DefaultBodyLimit::max(8 * 1024)),
        )
        .route(
            "/api/account/signin",
            post(signin).layer(DefaultBodyLimit::max(8 * 1024)),
        )
        .route(
            "/api/account/recover",
            post(send_recovery).layer(DefaultBodyLimit::max(8 * 1024)),
        )
        .route(
            "/api/account/new-password",
            post(reset_password).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/api/account/checkout", post(checkout))
        .route(
            "/api/billing/stripe-webhook",
            post(stripe_webhook).layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .fallback(dispatch)
        .layer(middleware::from_fn_with_state(
            gateway.local.clone(),
            public_transport,
        ))
        .with_state(gateway))
}

async fn account_config(State(gateway): State<Arc<Gateway>>) -> Json<serde_json::Value> {
    let mut value = json!({"enabled":true});
    if let Some(url) = gateway.auth.test_inbox() {
        value["test_inbox_url"] = json!(url);
    }
    if let Some(billing) = &gateway.billing {
        value["billing"] =
            json!({"credits":billing.credits,"amount":billing.amount,"currency":"usd","test":true});
    }
    Json(value)
}

async fn public_transport(State(app): State<Shared>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    let private = path.starts_with("/api/");
    if let Err(response) = transport_guard(&app, &request) {
        return security_headers(response, private);
    }
    if path.starts_with("/api/account/")
        && let Err(error) = same_origin(&app, &request)
    {
        return security_headers(error.into_response(), private);
    }
    security_headers(next.run(request).await, private)
}

async fn signup(
    State(gateway): State<Arc<Gateway>>,
    headers: HeaderMap,
    Json(credentials): Json<Credentials>,
) -> Api<Json<serde_json::Value>> {
    let redirect = format!("{}/studio", origin(&gateway.local, &headers));
    let verify = gateway
        .auth
        .signup(&gateway.local.client, credentials, &redirect)
        .await
        .map_err(|status| auth_error(status, true))?;
    let message = if verify {
        "Check your email to confirm your account, then sign in."
    } else {
        "Your account is ready. Sign in to open the studio."
    };
    Ok(Json(json!({"message":message,"confirm_email":verify})))
}

fn origin(app: &App, headers: &HeaderMap) -> String {
    let host = header_str(headers, "host").expect("validated host header");
    let scheme = if secure_host(app, headers) {
        "https"
    } else {
        "http"
    };
    format!("{scheme}://{host}")
}

async fn signin(
    State(gateway): State<Arc<Gateway>>,
    headers: HeaderMap,
    Json(credentials): Json<Credentials>,
) -> Api<Response> {
    let identity = gateway
        .auth
        .signin(&gateway.local.client, credentials)
        .await
        .map_err(|status| auth_error(status, false))?;
    let workspace = gateway.workspace(identity.user).await.map_err(internal)?;
    let time = editor::now();
    let access_lifetime = identity.lifetime.min(SESSION_SECONDS);
    let renewal = identity.refresh_token.map(|token| {
        Arc::new(tokio::sync::Mutex::new(Renewal {
            token,
            access_expires: time + access_lifetime,
        }))
    });
    let lifetime = if renewal.is_some() {
        SESSION_SECONDS
    } else {
        access_lifetime
    };
    let id = format!("{}.{}", identity.user, token());
    let mut sessions = workspace.app.sessions.lock().expect("account sessions");
    sessions.retain(|_, value| value.expires > time);
    if let Some(previous) = session_id(&headers) {
        sessions.remove(previous);
    }
    if sessions.len() >= 64 {
        return Err(failure(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many sessions are open.",
        ));
    }
    sessions.insert(
        id.clone(),
        Session {
            providers: HashMap::new(),
            active: None,
            expires: time + lifetime,
            renewal,
        },
    );
    Ok(cookie_response(&workspace.app, &headers, &id, lifetime))
}

async fn send_recovery(
    State(gateway): State<Arc<Gateway>>,
    headers: HeaderMap,
    Json(request): Json<RecoveryRequest>,
) -> Api<StatusCode> {
    let redirect = format!("{}/studio", origin(&gateway.local, &headers));
    gateway
        .auth
        .send_recovery(&gateway.local.client, request, &redirect)
        .await
        .map_err(|status| {
            if status == StatusCode::BAD_REQUEST {
                failure(status, "Enter your email address.")
            } else {
                auth_error(status, false)
            }
        })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn reset_password(
    State(gateway): State<Arc<Gateway>>,
    Json(request): Json<NewPassword>,
) -> Api<StatusCode> {
    let user = gateway
        .auth
        .reset_password(&gateway.local.client, request)
        .await
        .map_err(|status| match status {
            StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED => failure(
                status,
                "This reset link is invalid or expired. Request a new email.",
            ),
            StatusCode::UNPROCESSABLE_ENTITY => failure(
                status,
                "Choose a different password that meets your studio's requirements.",
            ),
            _ => auth_error(status, false),
        })?;
    if let Some(workspace) = gateway.workspaces.lock().await.get(&user) {
        workspace
            .app
            .sessions
            .lock()
            .expect("account sessions")
            .clear();
    }
    Ok(StatusCode::NO_CONTENT)
}

fn auth_error(status: StatusCode, signup: bool) -> ApiError {
    let message = match status {
        StatusCode::BAD_REQUEST => "Enter an email address and password.",
        StatusCode::TOO_MANY_REQUESTS => "Too many attempts. Try again shortly.",
        StatusCode::UNAUTHORIZED if signup => {
            "Could not create the account. Check your email and password."
        }
        StatusCode::UNAUTHORIZED => {
            "Check your email and password, and confirm your email address before signing in."
        }
        _ => "Studio sign-in is temporarily unavailable.",
    };
    failure(status, message)
}

impl Gateway {
    async fn restore(&self) -> Result<()> {
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let account = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::parse_str(name).ok());
            if let Some(account) = account
                && entry.file_type().await?.is_dir()
                && entry.path().join("studio.json").is_file()
                && self.workspace(account).await.is_err()
            {
                eprintln!("Could not reopen a saved studio account.");
            }
        }
        Ok(())
    }

    async fn workspace(&self, user: Uuid) -> Result<Workspace> {
        let mut workspaces = self.workspaces.lock().await;
        if let Some(workspace) = workspaces.get(&user) {
            return Ok(workspace.clone());
        }
        let app = open_workspace(
            &self.root.join(user.to_string()),
            self.local.hosts.clone(),
            self.local.worker.clone(),
            self.local.client.clone(),
            self.local.home_assets.clone(),
            self.local.fixture,
            WorkspaceMode::Account {
                initial_credits: self.initial_credits,
            },
        )
        .await?;
        start_workspace(&app);
        let workspace = Workspace {
            router: workspace_routes(app.clone()),
            app,
            checkout_attempts: Arc::new(Mutex::new(Vec::new())),
        };
        workspaces.insert(user, workspace.clone());
        Ok(workspace)
    }

    async fn browser(&self, headers: &HeaderMap) -> Option<(Uuid, Workspace)> {
        let workspaces = self.workspaces.lock().await;
        browser_workspace(&workspaces, headers)
            .map(|(account, workspace)| (account, workspace.clone()))
    }

    async fn signed_in(&self, headers: &HeaderMap) -> Api<Option<(Uuid, Workspace)>> {
        let Some((account, workspace)) = self.browser(headers).await else {
            return Ok(None);
        };
        if let Some(renewal) =
            with_session(&workspace.app, headers, |session| session.renewal.clone())?
        {
            let mut renewal = renewal.lock().await;
            with_session(&workspace.app, headers, |_| ())?;
            if renewal.access_expires <= editor::now() {
                let response = self.auth.refresh(&self.local.client, &renewal.token).await;
                let fresh = match response {
                    Ok(identity) if identity.user == account => {
                        identity.refresh_token.map(|token| Renewal {
                            token,
                            access_expires: editor::now() + identity.lifetime.min(SESSION_SECONDS),
                        })
                    }
                    Err(status) if status != StatusCode::UNAUTHORIZED => {
                        return Err(auth_error(status, false));
                    }
                    _ => None,
                };
                let Some(fresh) = fresh else {
                    let id = session_id(headers).unwrap_or_default();
                    workspace
                        .app
                        .sessions
                        .lock()
                        .expect("account sessions")
                        .remove(id);
                    return Err(failure(
                        StatusCode::UNAUTHORIZED,
                        "Your session ended. Sign in again.",
                    ));
                };
                with_session(&workspace.app, headers, |_| ())?;
                *renewal = fresh;
            }
        }
        Ok(Some((account, workspace)))
    }
}

fn browser_workspace<'a>(
    workspaces: &'a HashMap<Uuid, Workspace>,
    headers: &HeaderMap,
) -> Option<(Uuid, &'a Workspace)> {
    let (account, _) = session_id(headers)?.split_once('.')?;
    let account = Uuid::parse_str(account).ok()?;
    let workspace = workspaces.get(&account)?;
    with_session(&workspace.app, headers, |_| ()).ok()?;
    Some((account, workspace))
}

async fn checkout(
    State(gateway): State<Arc<Gateway>>,
    headers: HeaderMap,
) -> Api<Json<serde_json::Value>> {
    let (account, workspace) = gateway
        .signed_in(&headers)
        .await?
        .ok_or_else(|| failure(StatusCode::UNAUTHORIZED, "Sign in to buy render credits."))?;
    let billing = gateway
        .billing
        .as_ref()
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Stripe checkout is unavailable."))?;
    {
        let time = editor::now();
        let mut attempts = workspace
            .checkout_attempts
            .lock()
            .expect("checkout attempts");
        attempts.retain(|attempt| time.saturating_sub(*attempt) < 60);
        if attempts.len() >= 6 {
            return Err(failure(
                StatusCode::TOO_MANY_REQUESTS,
                "Try opening checkout in a minute.",
            ));
        }
        attempts.push(time);
    }
    let session = billing
        .checkout(
            &gateway.local.client,
            account,
            &origin(&workspace.app, &headers),
        )
        .await
        .map_err(|_| {
            failure(
                StatusCode::BAD_GATEWAY,
                "Could not open Stripe test checkout.",
            )
        })?;
    with_session(&workspace.app, &headers, |_| ())?;
    workspace
        .app
        .store
        .update(|db| {
            db.register_checkout(session.id, billing.amount, billing.credits);
            Ok(())
        })
        .map_err(internal)?;
    Ok(Json(json!({"url":session.url})))
}

async fn stripe_webhook(
    State(gateway): State<Arc<Gateway>>,
    headers: HeaderMap,
    body: Bytes,
) -> Api<StatusCode> {
    let billing = gateway
        .billing
        .as_ref()
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Stripe checkout is unavailable."))?;
    let signature = header_str(&headers, "stripe-signature")
        .ok_or_else(|| failure(StatusCode::BAD_REQUEST, "Missing Stripe signature."))?;
    let payment = billing
        .payment(signature, &body, editor::now())
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "Invalid Stripe notification."))?;
    if let Some(payment) = payment
        && gateway
            .root
            .join(payment.account.to_string())
            .join("studio.json")
            .is_file()
    {
        let workspace = gateway.workspace(payment.account).await.map_err(internal)?;
        workspace
            .app
            .store
            .update(|db| db.settle_checkout(&payment.checkout, payment.amount))
            .map_err(internal)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn dispatch(State(gateway): State<Arc<Gateway>>, request: Request) -> Response {
    let path = request.uri().path();
    if !path.starts_with("/api/") {
        return gateway
            .public_routes
            .clone()
            .oneshot(request)
            .await
            .expect("infallible workspace router");
    }
    if path == "/api/login" {
        return failure(StatusCode::FORBIDDEN, "Sign in with your studio account.").into_response();
    }
    if !request.headers().contains_key(header::AUTHORIZATION)
        && let Err(error) = same_origin(&gateway.local, &request)
    {
        return error.into_response();
    }
    let browser = if path == "/api/logout" {
        gateway.browser(request.headers()).await
    } else {
        match gateway.signed_in(request.headers()).await {
            Ok(browser) => browser,
            Err(error) => return error.into_response(),
        }
    };
    let workspace = match browser {
        Some((_, workspace)) => Some(workspace),
        None if request.headers().contains_key(header::AUTHORIZATION) => {
            let accounts = gateway.workspaces.lock().await;
            accounts
                .values()
                .find(|workspace| mcp_authorized(&workspace.app, request.headers(), path))
                .cloned()
        }
        None => None,
    };
    let Some(workspace) = workspace else {
        return failure(StatusCode::UNAUTHORIZED, "Sign in to open the studio.").into_response();
    };
    workspace
        .router
        .oneshot(request)
        .await
        .expect("infallible workspace router")
}
