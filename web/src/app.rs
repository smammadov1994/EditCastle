use crate::clock;
use crate::{
    audio, auto_edit,
    director_preview::Feed,
    director_prompt,
    editor::{
        self, ChargeState, Citation, Clip, Message, Project, RenderReceipt, Run, Source, Store,
        VisibleEdit,
    },
    export_queue, library, mcp_activity, media, playback,
    provider::{Action, Agent, Name, Provider},
    public_web,
    render::{Status as WorkerStatus, Worker},
    room, search, studio_history, timing, tools,
};
use anyhow::{Context, Result, bail, ensure};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Multipart, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::{io::AsyncWriteExt, net::TcpListener, sync::Semaphore};
use tower::ServiceExt;
use tower_http::services::ServeFile;
use uuid::Uuid;

#[path = "account_gateway.rs"]
mod account_gateway;

pub(crate) const MAX_IMPORT: usize = 256 * 1024 * 1024;
const COOKIE: &str = "editroom_session";
const SESSION_SECONDS: u64 = 12 * 60 * 60;
const MAX_LOCAL_TEST_CREDITS: u32 = 5_000;

type Shared = Arc<App>;
type Api<T> = std::result::Result<T, ApiError>;

struct App {
    store: Store,
    worker: Worker,
    client: Client,
    owner: Option<String>,
    sessions: Mutex<HashMap<String, Session>>,
    playback: playback::Hub,
    mcp_activity: mcp_activity::Hub,
    director_feeds: Mutex<HashMap<Uuid, Arc<Feed>>>,
    recovering_render: Mutex<Option<Uuid>>,
    failures: Mutex<Vec<u64>>,
    hosts: Vec<String>,
    home_assets: Option<PathBuf>,
    fixture: bool,
    attached_agents: Mutex<crate::attached_agents::Access>,
    attached_cli_change: tokio::sync::Mutex<()>,
    export_slot: Arc<Semaphore>,
    exports: export_queue::Queue,
    import_slot: Semaphore,
    grade_slots: Semaphore,
    overlay_slots: Arc<Semaphore>,
    inbox_state: tokio::sync::Mutex<library::InboxState>,
}

struct Session {
    providers: HashMap<Name, Provider>,
    active: Option<Name>,
    expires: u64,
    renewal: Option<Arc<tokio::sync::Mutex<crate::accounts::Renewal>>>,
}

enum WorkspaceMode {
    Standalone,
    AccountGateway,
    Account { initial_credits: u32 },
}

impl Session {
    fn provider(&self) -> Option<Provider> {
        self.providers.get(&self.active?).cloned()
    }
}

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn bad(error: anyhow::Error) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, error.to_string())
}

fn internal(_: anyhow::Error) -> ApiError {
    ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "The studio could not save that operation.".into(),
    )
}

fn failure(code: StatusCode, text: &str) -> ApiError {
    ApiError(code, text.into())
}

fn token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn session_id(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|values| values.split(';'))
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(name, value)| (name == COOKIE).then_some(value))
}

fn with_session<T>(app: &App, headers: &HeaderMap, read: impl FnOnce(&Session) -> T) -> Api<T> {
    let id = session_id(headers).unwrap_or_default();
    let mut sessions = app.sessions.lock().expect("sessions");
    sessions.retain(|_, value| value.expires > editor::now());
    sessions
        .get(id)
        .map(read)
        .ok_or_else(|| failure(StatusCode::UNAUTHORIZED, "Sign in to open the studio."))
}

fn mcp_authorized(app: &App, headers: &HeaderMap, path: &str) -> bool {
    path.strip_prefix("/api/projects/")
        .and_then(|tail| {
            tail.strip_suffix("/mcp")
                .or_else(|| tail.strip_suffix("/mcp/upload"))
        })
        .is_some_and(|project| Uuid::parse_str(project).is_ok())
        && attached_owner(app, headers).is_some()
}

fn attached_owner(app: &App, headers: &HeaderMap) -> Option<mcp_activity::Owner> {
    let received = header_str(headers, "authorization")?.strip_prefix("Bearer ")?;
    app.attached_agents
        .lock()
        .expect("attached agent access")
        .authorize(received)
        .map(mcp_activity::Owner::Bearer)
}

fn secure_host(app: &App, headers: &HeaderMap) -> bool {
    header_str(headers, "host")
        .is_some_and(|host| app.hosts.iter().skip(2).any(|value| value == host))
}

fn transport_guard(app: &App, request: &Request) -> std::result::Result<(), Response> {
    let headers = request.headers();
    let method = request.method();
    let host = header_str(headers, "host").unwrap_or_default();
    if !app.hosts.iter().any(|allowed| allowed == host) {
        return Err(failure(
            StatusCode::MISDIRECTED_REQUEST,
            "This host is not configured for EditCastle.",
        )
        .into_response());
    }
    let secure = secure_host(app, headers);
    if secure {
        // Studio listens on loopback. The tunnel supplies the visitor's scheme.
        let mut schemes = headers.get_all("x-forwarded-proto").iter();
        let scheme = schemes.next().and_then(|value| value.to_str().ok());
        match (scheme, schemes.next()) {
            (Some("https"), None) => {}
            (Some("http"), None)
                if matches!(*method, axum::http::Method::GET | axum::http::Method::HEAD) =>
            {
                let path = request
                    .uri()
                    .path_and_query()
                    .map_or("/", |path| path.as_str());
                return Err(Redirect::permanent(&format!("https://{host}{path}")).into_response());
            }
            _ => {
                return Err(failure(
                    StatusCode::FORBIDDEN,
                    "Open the studio over HTTPS through its configured tunnel.",
                )
                .into_response());
            }
        }
    }
    Ok(())
}

fn same_origin(app: &App, request: &Request) -> Api<()> {
    let headers = request.headers();
    let method = request.method();
    if method != axum::http::Method::GET && method != axum::http::Method::HEAD {
        let host = header_str(headers, "host").unwrap_or_default();
        let scheme = if secure_host(app, headers) {
            "https"
        } else {
            "http"
        };
        if header_str(headers, "origin") != Some(format!("{scheme}://{host}").as_str())
            || header_str(headers, "sec-fetch-site").is_some_and(|site| site != "same-origin")
        {
            return Err(failure(
                StatusCode::FORBIDDEN,
                "Open the studio directly to make changes.",
            ));
        }
    }
    Ok(())
}

async fn guard(State(app): State<Shared>, request: Request, next: Next) -> Response {
    if let Err(response) = transport_guard(&app, &request) {
        return response;
    }
    let headers = request.headers();
    let mcp = mcp_authorized(&app, headers, request.uri().path());
    if !mcp && let Err(error) = same_origin(&app, &request) {
        return error.into_response();
    }
    let private = request.uri().path().starts_with("/api/");
    if private
        && !mcp
        && !matches!(request.uri().path(), "/api/login" | "/api/account/config")
        && let Err(error) = with_session(&app, headers, |_| ())
    {
        return error.into_response();
    }
    security_headers(next.run(request).await, private)
}

fn security_headers(mut response: Response, private: bool) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' blob:; media-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'"
    ));
    if private {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    response
}

pub async fn serve() -> Result<()> {
    let address: SocketAddr = env::var("EDITROOM_BIND")
        .unwrap_or_else(|_| "127.0.0.1:4324".into())
        .parse()
        .context("Invalid EditCastle bind address")?;
    ensure!(
        address.ip().is_loopback(),
        "EditCastle binds to loopback. Use a tunnel for remote access."
    );
    let path = env::var_os("EDITROOM_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("studio-data"));
    let path = if path.is_absolute() {
        path
    } else {
        env::current_dir()?.join(path)
    };
    let fixture = env::var_os("EDITROOM_FIXTURE_PLAN").is_some();
    let mut hosts = vec![address.to_string(), format!("localhost:{}", address.port())];
    if let Ok(domain) = env::var("EDITROOM_PUBLIC_HOST") {
        ensure!(
            !fixture,
            "A test planner cannot run with a public hostname."
        );
        ensure!(
            !domain.is_empty()
                && domain.split('.').count() >= 2
                && domain.bytes().all(|ch| ch.is_ascii_lowercase()
                    || ch.is_ascii_digit()
                    || ch == b'.'
                    || ch == b'-'),
            "EDITROOM_PUBLIC_HOST must be one lowercase HTTPS hostname without a port."
        );
        hosts.push(domain);
    }
    let public = hosts.len() > 2;
    let accounts = crate::accounts::Accounts::configured(public)?;
    ensure!(
        !public || accounts.is_some(),
        "Configure Supabase accounts before enabling a public hostname."
    );
    let local_test_credits = match env::var_os("EDITROOM_LOCAL_TEST_CREDITS") {
        Some(configured) => {
            ensure!(
                !public
                    && accounts
                        .as_ref()
                        .is_some_and(crate::accounts::Accounts::is_loopback_preview),
                "Local test credits need a private loopback Supabase preview."
            );
            configured
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|amount| (1..=MAX_LOCAL_TEST_CREDITS).contains(amount))
                .context("Set EDITROOM_LOCAL_TEST_CREDITS to a value between 1 and 5,000.")?
        }
        None => 0,
    };
    let billing = crate::billing::Billing::configured(fixture)?;
    ensure!(
        billing.is_none() || accounts.is_some(),
        "Configure studio accounts before enabling Stripe."
    );
    let worker = Worker::new(
        &env::var("EDITROOM_COMFY_URL").unwrap_or_else(|_| "http://127.0.0.1:48188".into()),
    )?;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let assets = env::var_os("EDITROOM_HOME_ASSETS").map(PathBuf::from);
    let mode = if accounts.is_some() {
        WorkspaceMode::AccountGateway
    } else {
        WorkspaceMode::Standalone
    };
    let app = open_workspace(&path, hosts, worker, client, assets, fixture, mode).await?;
    app.worker.maintain_tunnel();
    let routes = if let Some(accounts) = accounts {
        account_gateway::routes(
            app,
            accounts,
            billing,
            path.join("accounts"),
            local_test_credits,
        )
        .await?
    } else {
        start_workspace(&app);
        workspace_routes(app)
    };
    let listener = TcpListener::bind(address).await?;
    println!("EditCastle Director is listening on http://{address}");
    axum::serve(listener, routes).await?;
    Ok(())
}

async fn open_workspace(
    path: &std::path::Path,
    hosts: Vec<String>,
    worker: Worker,
    client: Client,
    home_assets: Option<PathBuf>,
    fixture: bool,
    mode: WorkspaceMode,
) -> Result<Shared> {
    let standalone = matches!(mode, WorkspaceMode::Standalone);
    let initial_account_credits = match mode {
        WorkspaceMode::Account { initial_credits } => Some(initial_credits),
        _ => None,
    };
    let store = Store::open_workspace(path, initial_account_credits)?;
    let exports = export_queue::Queue::open(&store).await?;
    let owner = if standalone {
        let owner_path = path.join("access-key");
        if !owner_path.exists() {
            editor::private_write(&owner_path, token().as_bytes())?;
        }
        let owner = fs::read_to_string(owner_path)?.trim().to_owned();
        ensure!(
            owner.len() >= 32,
            "The owner access-key needs at least 32 characters."
        );
        Some(owner)
    } else {
        None
    };
    let attached_agents = if matches!(mode, WorkspaceMode::AccountGateway) {
        crate::attached_agents::Access::disabled(path)
    } else {
        let override_path = if standalone {
            env::var_os("EDITROOM_MCP_KEY_FILE").map(PathBuf::from)
        } else {
            None
        };
        crate::attached_agents::Access::open(path, override_path, owner.as_deref())?
    };
    Ok(Arc::new(App {
        store,
        worker,
        client,
        owner,
        hosts,
        fixture,
        attached_agents: Mutex::new(attached_agents),
        attached_cli_change: tokio::sync::Mutex::new(()),
        sessions: Mutex::new(HashMap::new()),
        playback: playback::Hub::default(),
        mcp_activity: mcp_activity::Hub::default(),
        director_feeds: Mutex::new(HashMap::new()),
        recovering_render: Mutex::new(None),
        failures: Mutex::new(vec![]),
        home_assets,
        export_slot: Arc::new(Semaphore::new(1)),
        exports,
        import_slot: Semaphore::new(1),
        grade_slots: Semaphore::new(2),
        overlay_slots: Arc::new(Semaphore::new(3)),
        inbox_state: tokio::sync::Mutex::new(library::InboxState::default()),
    }))
}

fn start_workspace(app: &Shared) {
    tokio::spawn(reconcile(app.clone()));
    tokio::spawn(watch_inbox(app.clone()));
    let export_app = app.clone();
    tokio::spawn(async move {
        export_app
            .exports
            .serve(&export_app.store, &export_app.export_slot)
            .await
    });
    for index in [search::Index::Vision, search::Index::Speech] {
        let app = app.clone();
        tokio::spawn(async move { search::index_library(&app.store, index).await });
    }
}

fn workspace_routes(app: Shared) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { Html(include_str!("../static/home.html")) }),
        )
        .route("/landing/{*name}", get(landing_asset))
        .route(
            "/home.css",
            get(|| async {
                (
                    [("content-type", "text/css; charset=utf-8")],
                    include_str!("../static/home.css"),
                )
            }),
        )
        .route(
            "/studio",
            get(|| async { Html(include_str!("../static/studio.html")) }),
        )
        .route(
            "/home.js",
            get(|| async { js(include_str!("../static/home.js")) }),
        )
        .route(
            "/device-folders.js",
            get(|| async { js(include_str!("../static/device-folders.js")) }),
        )
        .route(
            "/grade-preview.js",
            get(|| async { js(include_str!("../static/grade-preview.js")) }),
        )
        .route(
            "/timeline-view.js",
            get(|| async { js(include_str!("../static/timeline-view.js")) }),
        )
        .route(
            "/filmstrip.js",
            get(|| async { js(include_str!("../static/filmstrip.js")) }),
        )
        .route(
            "/shortcuts.js",
            get(|| async { js(include_str!("../static/shortcuts.js")) }),
        )
        .route(
            "/magic-show.js",
            get(|| async { js(include_str!("../static/magic-show.js")) }),
        )
        .route(
            "/studio.js",
            get(|| async { js(include_str!("../static/studio.js")) }),
        )
        .route(
            "/studio.css",
            get(|| async {
                (
                    [("content-type", "text/css; charset=utf-8")],
                    include_str!("../static/studio.css"),
                )
            }),
        )
        .route("/api/login", post(login))
        .route(
            "/api/account/config",
            get(|| async { Json(json!({"enabled":false})) }),
        )
        .route("/api/logout", post(logout))
        .route("/api/state", get(state))
        .route("/api/settings/saving", post(saving_settings))
        .route(
            "/api/attached-agents",
            get(attached_settings).post(update_attached_settings),
        )
        .route("/api/attached-agents/key", post(copy_attached_key))
        .route("/api/title-fonts", get(title_fonts))
        .route("/api/provider", post(connect))
        .route("/api/provider/check", post(check_provider))
        .route("/api/provider/disconnect", post(disconnect))
        .route("/api/projects", post(new_project))
        .route(
            "/api/projects/open-room",
            post(open_room).layer(DefaultBodyLimit::max(
                room::MAX_BUNDLE as usize + 1024 * 1024,
            )),
        )
        .route("/api/projects/{project}", post(update_project))
        .route("/api/projects/{project}/copy", post(copy_project))
        .route("/api/projects/{project}/save", post(save_project))
        .route("/api/projects/{project}/revert", post(revert_project))
        .route(
            "/api/projects/{project}/playback",
            get(watch_playback).post(browser_playback),
        )
        .route("/api/projects/{project}/room.zip", get(save_room))
        .route("/api/projects/{project}/captions", get(caption_preview))
        .route("/api/projects/{project}/grade-preview", post(grade_preview))
        .route("/api/projects/{project}/title-preview", post(title_preview))
        .route(
            "/api/projects/{project}/caption-preview",
            post(caption_overlay_preview),
        )
        .route("/api/projects/{project}/trim-preview", post(trim_preview))
        .route(
            "/api/projects/{project}/retime-preview",
            post(retime_preview),
        )
        .route("/api/projects/{project}/media-search", get(media_search))
        .route("/api/projects/{project}/media-folders", post(media_folders))
        .route(
            "/api/projects/{project}/device-folders/{folder}",
            get(device_folder_state)
                .post(sync_device_folder)
                .layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route(
            "/api/projects/{project}/device-folders/{folder}/file",
            post(upload_device_file),
        )
        .route("/api/projects/{project}/media-inbox", get(media_inbox))
        .route("/api/projects/{project}/audio-mix", get(audio_mix))
        .route("/api/projects/{project}/renderer", post(set_renderer))
        .route(
            "/api/projects/{project}/renderer/check",
            get(check_renderer),
        )
        .route(
            "/api/projects/{project}/lut",
            post(load_lut).layer(DefaultBodyLimit::max(14 * 1024 * 1024)),
        )
        .route("/api/projects/{project}/director/stop", post(stop_run))
        .route(
            "/api/projects/{project}/director/recover/{job}",
            post(recover_shot),
        )
        .route(
            "/api/projects/{project}/director/runs/{run}/append-reviewed",
            post(append_reviewed_shots),
        )
        .route(
            "/api/projects/{project}/director/draft",
            get(director_draft),
        )
        .route("/api/projects/{project}/director", post(start_run))
        .route(
            "/api/projects/{project}/mcp-bundle",
            get(download_mcp_bundle),
        )
        .route(
            "/api/projects/{project}/mcp-cli-setup",
            get(attached_cli_setup).post(change_attached_cli),
        )
        .route(
            "/api/projects/{project}/mcp-cli-registration",
            get(attached_cli_registration),
        )
        .route("/api/projects/{project}/auto-edit", post(start_auto_edit))
        .route("/api/projects/{project}/import", post(import))
        .route(
            "/api/projects/{project}/media/{asset}/download",
            get(download_source),
        )
        .route(
            "/api/projects/{project}/sequence-offer",
            post(decide_sequence_offer),
        )
        .route("/api/projects/{project}/export", post(export))
        .route("/api/media/{id}", get(media_file))
        .route(
            "/api/projects/{project}/audio-preview/{clip}/{key}",
            get(timed_audio_file),
        )
        .route("/api/thumb/{id}", get(thumbnail))
        .route("/api/waveform/{id}", get(waveform_file))
        .route("/api/waveform-levels/{id}", get(waveform_levels))
        .route("/api/exports", get(export_list))
        .route("/api/exports/clear", post(clear_exports))
        .route("/api/exports/{id}/cancel", post(cancel_export))
        .route("/api/exports/{id}", get(export_download))
        .route(
            "/api/projects/{project}/mcp",
            post(mcp_rpc)
                .delete(mcp_disconnect)
                .layer(DefaultBodyLimit::max(4 * 1024 * 1024)),
        )
        .route(
            "/api/projects/{project}/mcp/upload",
            post(create_agent_upload_folder).put(agent_upload),
        )
        .route(
            "/api/projects/{project}/mcp-activity",
            get(mcp_activity_feed),
        )
        .route("/api/credits", post(grant))
        .route("/{asset}", get(asset))
        .layer(DefaultBodyLimit::max(400 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

fn js(content: &'static str) -> impl IntoResponse {
    (
        [("content-type", "text/javascript; charset=utf-8")],
        content,
    )
}

async fn asset(State(app): State<Shared>, Path(name): Path<String>, request: Request) -> Response {
    use crate::assets;
    if let Some(data) = assets::find(assets::STUDIO, &name) {
        return assets::serve(&request, &name, data);
    }
    let Some(data) = assets::find(assets::HOME, &name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(folder) = &app.home_assets {
        return ServeFile::new(folder.join(name))
            .oneshot(request)
            .await
            .expect("file service")
            .into_response();
    }
    assets::serve(&request, &name, data)
}

async fn landing_asset(Path(name): Path<String>, request: Request) -> Response {
    use crate::assets;
    match assets::find(assets::LANDING, &name) {
        Some(data) => assets::serve(&request, &name, data),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct Login {
    key: String,
}

async fn login(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> Api<Response> {
    let owner = app
        .owner
        .as_deref()
        .ok_or_else(|| failure(StatusCode::FORBIDDEN, "Sign in with your studio account."))?;
    let time = editor::now();
    let mut failures = app.failures.lock().expect("login attempts");
    failures.retain(|attempt| time.saturating_sub(*attempt) < 60);
    if failures.len() >= 12 {
        return Err(failure(
            StatusCode::TOO_MANY_REQUESTS,
            "Try signing in again in a minute.",
        ));
    }
    if !bool::from(input.key.as_bytes().ct_eq(owner.as_bytes())) {
        failures.push(time);
        return Err(failure(
            StatusCode::UNAUTHORIZED,
            "That studio access key is incorrect.",
        ));
    }
    failures.clear();
    let id = token();
    let mut sessions = app.sessions.lock().expect("sessions");
    sessions.retain(|_, value| value.expires > time);
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
            expires: time + SESSION_SECONDS,
            renewal: None,
        },
    );
    Ok(cookie_response(&app, &headers, &id, SESSION_SECONDS))
}

fn cookie_response(app: &App, headers: &HeaderMap, id: &str, max_age: u64) -> Response {
    let ssl = if secure_host(app, headers) {
        "; Secure"
    } else {
        ""
    };
    let value = format!("{COOKIE}={id}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{ssl}");
    ([(header::SET_COOKIE, value)], StatusCode::NO_CONTENT).into_response()
}

async fn logout(State(app): State<Shared>, headers: HeaderMap) -> Response {
    if let Some(id) = session_id(&headers) {
        app.sessions.lock().expect("sessions").remove(id);
    }
    cookie_response(&app, &headers, "", 0)
}

#[derive(Deserialize)]
struct Connect {
    provider: Name,
    key: Option<String>,
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Deserialize)]
struct Disconnect {
    provider: Name,
}

async fn check_provider(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<Connect>,
) -> Api<Json<crate::provider::ModelCheck>> {
    if !matches!(input.provider, Name::OpenAi | Name::Claude) {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Choose ChatGPT or Claude to check model access.",
        ));
    }
    let candidate = with_session(&app, &headers, |session| {
        Provider::configuration(
            input.provider,
            session.providers.get(&input.provider),
            input.key,
            input.model,
            input.effort,
        )
    })?
    .map_err(bad)?;
    let result = candidate.check_model(&app.client).await;
    with_session(&app, &headers, |_| ())?;
    Ok(Json(result))
}

async fn connect(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<Connect>,
) -> Api<StatusCode> {
    if input.provider == Name::Fixture && !app.fixture {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Choose ChatGPT or Claude.",
        ));
    }
    if input.provider == Name::Local && !crate::provider::builtin_local() {
        crate::provider::local_config().map_err(bad)?;
    }
    let mut sessions = app.sessions.lock().expect("sessions");
    let session = sessions
        .get_mut(session_id(&headers).unwrap_or_default())
        .ok_or_else(|| {
            failure(
                StatusCode::UNAUTHORIZED,
                "Sign in again to connect a provider.",
            )
        })?;
    let provider = Provider::configuration(
        input.provider,
        session.providers.get(&input.provider),
        input.key,
        input.model,
        input.effort,
    )
    .map_err(bad)?;
    session.providers.insert(input.provider, provider);
    session.active = Some(input.provider);
    Ok(StatusCode::NO_CONTENT)
}

async fn disconnect(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<Disconnect>,
) -> Api<StatusCode> {
    let mut sessions = app.sessions.lock().expect("sessions");
    let session = sessions
        .get_mut(session_id(&headers).unwrap_or_default())
        .ok_or_else(|| {
            failure(
                StatusCode::UNAUTHORIZED,
                "Sign in again to change a provider.",
            )
        })?;
    session.providers.remove(&input.provider);
    if session.active == Some(input.provider) {
        session.active = None;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn state(State(app): State<Shared>, headers: HeaderMap) -> Api<Json<serde_json::Value>> {
    let (active, connections) = with_session(&app, &headers, |session| {
        (
            session.active,
            session
                .providers
                .values()
                .map(Provider::public)
                .collect::<Vec<_>>(),
        )
    })?;
    let models = json!({
        "open_ai":{"default":Name::OpenAi.default_model(),"options":Name::OpenAi.models()},
        "claude":{"default":Name::Claude.default_model(),"options":Name::Claude.models()},
        "local":{"default":Name::Local.default_model(),"options":Name::Local.models()},
    });
    let recovering = *app.recovering_render.lock().expect("render recovery");
    Ok(Json(app.store.read(|db| json!({
        "projects": db.projects.iter().map(|project| {
            let transport = app.playback.view(project);
            let mut state = project.public();
            state["sequence"]["playhead"] = json!(transport.at);
            state["sequence"]["playing"] = json!(transport.playing);
            state["has_unsaved_changes"] = json!(db.has_unsaved_changes(project.id));
            state
        }).collect::<Vec<_>>(), "runs": db.runs.iter().rev().take(30).collect::<Vec<_>>(),
        "available": db.available(), "charges": db.charges.iter().rev().take(100).collect::<Vec<_>>(),
        "recoverable_renders": db.recoverable_renders().take(100).map(|(job, receipt)| json!({
            "id":job, "project":receipt.project, "title":receipt.title, "issue":receipt.issue,
            "active":recovering==Some(job),
        })).collect::<Vec<_>>(),
        "provider": active, "provider_connections": connections, "provider_models": models,
        "builtin_local": crate::provider::builtin_local(), "autosave": db.autosave
    }))))
}

#[derive(Deserialize)]
struct SavingSettings {
    autosave: bool,
}

async fn saving_settings(
    State(app): State<Shared>,
    Json(settings): Json<SavingSettings>,
) -> Api<StatusCode> {
    app.store
        .set_autosave(settings.autosave)
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn save_project(State(app): State<Shared>, Path(id): Path<Uuid>) -> Api<StatusCode> {
    app.store.save_project(id).map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RevertVersion {
    revision: u64,
    library_revision: u64,
}

async fn revert_project(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(version): Json<RevertVersion>,
) -> Api<StatusCode> {
    app.store
        .revert_project(id, version.revision, version.library_revision)
        .map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn title_fonts() -> Api<Json<&'static [String]>> {
    tokio::task::spawn_blocking(media::title_families)
        .await
        .map(Json)
        .map_err(|error| internal(error.into()))
}

async fn attached_settings(State(app): State<Shared>) -> Json<serde_json::Value> {
    Json(attached_settings_json(&app))
}

fn attached_settings_json(app: &App) -> serde_json::Value {
    let access = app.attached_agents.lock().expect("attached agent access");
    json!({"access":access.settings(),"active":app.mcp_activity.active_attached()})
}

#[derive(Deserialize)]
struct AttachedSettingsRequest {
    enabled: bool,
}

async fn update_attached_settings(
    State(app): State<Shared>,
    Json(request): Json<AttachedSettingsRequest>,
) -> Api<Json<serde_json::Value>> {
    tokio::task::spawn_blocking(move || {
        {
            let mut access = app.attached_agents.lock().expect("attached agent access");
            access
                .set_enabled(request.enabled, token)
                .map_err(internal)?;
            if !request.enabled {
                app.mcp_activity.disconnect_attached();
            }
        }
        Ok(Json(attached_settings_json(&app)))
    })
    .await
    .map_err(|error| internal(error.into()))?
}

async fn copy_attached_key(State(app): State<Shared>) -> Api<Response> {
    let access = app.attached_agents.lock().expect("attached agent access");
    let key = access
        .key()
        .ok_or_else(|| failure(StatusCode::CONFLICT, "Enable attached agents first."))?;
    Ok(key.to_owned().into_response())
}

async fn download_mcp_bundle(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    headers: HeaderMap,
) -> Api<Response> {
    let connection = attached_project(&app, project, &headers)?;
    let bundle = tokio::task::spawn_blocking(move || {
        let (executable, key_path) = connection.local_paths(cfg!(target_os = "macos"));
        crate::mcp_bundle::zip(
            project,
            &connection.title,
            &connection.endpoint,
            executable.as_deref(),
            key_path.as_deref(),
            exposed_tools(false),
        )
    })
    .await
    .map_err(|error| internal(error.into()))?
    .map_err(internal)?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"EditCastle.mcpb\"",
            ),
        ],
        bundle,
    )
        .into_response())
}

#[derive(Deserialize)]
struct AttachedCliQuery {
    client: crate::mcp_setup::Client,
}

async fn attached_cli_setup(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<AttachedCliQuery>,
) -> Api<Json<crate::mcp_setup::Commands>> {
    let connection = attached_project(&app, project, &headers)?;
    let setup = connection.cli_setup(project, query.client).await?;
    Ok(Json(setup.commands()))
}

async fn attached_cli_registration(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    headers: HeaderMap,
    Query(query): Query<AttachedCliQuery>,
) -> Api<Json<crate::mcp_setup::Registration>> {
    let connection = attached_project(&app, project, &headers)?;
    if !connection.local {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Check registration on the client machine.",
        ));
    }
    let setup = connection.cli_setup(project, query.client).await?;
    Ok(Json(setup.registration().await))
}

#[derive(Deserialize)]
struct AttachedCliChange {
    client: crate::mcp_setup::Client,
    action: crate::mcp_setup::Action,
}

async fn change_attached_cli(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    headers: HeaderMap,
    Json(change): Json<AttachedCliChange>,
) -> Api<StatusCode> {
    let connection = attached_project(&app, project, &headers)?;
    if !connection.local {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Run the setup command on the client machine.",
        ));
    }
    let _slot = app.attached_cli_change.try_lock().map_err(|_| {
        failure(
            StatusCode::CONFLICT,
            "Studio is changing another client connection.",
        )
    })?;
    let setup = connection.cli_setup(project, change.client).await?;
    setup.apply(change.action).await.map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

struct AttachedProject {
    title: String,
    endpoint: String,
    local: bool,
    key_path: PathBuf,
}

impl AttachedProject {
    async fn cli_setup(
        self,
        project: Uuid,
        client: crate::mcp_setup::Client,
    ) -> Api<crate::mcp_setup::Setup> {
        tokio::task::spawn_blocking(move || {
            let (executable, key_path) = self.local_paths(true);
            crate::mcp_setup::setup(
                client,
                project,
                &self.endpoint,
                executable.as_deref(),
                key_path.as_deref(),
            )
        })
        .await
        .map_err(|error| internal(error.into()))
    }

    fn local_paths(&self, allowed: bool) -> (Option<PathBuf>, Option<PathBuf>) {
        if self.local && allowed {
            (
                env::current_exe().ok(),
                fs::canonicalize(&self.key_path).ok(),
            )
        } else {
            (None, None)
        }
    }
}

fn attached_project(app: &App, project: Uuid, headers: &HeaderMap) -> Api<AttachedProject> {
    let Some(title) = app.store.read(|db| {
        db.projects
            .iter()
            .find(|item| item.id == project)
            .map(|item| item.title.clone())
    }) else {
        return Err(failure(StatusCode::NOT_FOUND, "Project not found."));
    };
    let key_path = {
        let access = app.attached_agents.lock().expect("attached agent access");
        if access.key().is_none() {
            return Err(failure(
                StatusCode::CONFLICT,
                "Enable attached agents first.",
            ));
        }
        access.key_file().to_path_buf()
    };
    let host = header_str(headers, "host").unwrap_or_default();
    let secure = secure_host(app, headers);
    let loopback = host.starts_with("localhost:")
        || host
            .parse::<SocketAddr>()
            .is_ok_and(|address| address.ip().is_loopback());
    if !secure && !loopback {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Use HTTPS to connect to a remote Studio.",
        ));
    }
    let local = !app.store.account && !secure && loopback;
    let scheme = if secure { "https" } else { "http" };
    let endpoint = format!("{scheme}://{host}/api/projects/{project}/mcp");
    Ok(AttachedProject {
        title,
        endpoint,
        local,
        key_path,
    })
}

#[derive(Deserialize)]
struct PlaybackCursor {
    after: Option<u64>,
}

async fn watch_playback(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Query(cursor): Query<PlaybackCursor>,
    headers: HeaderMap,
) -> Api<Response> {
    let mut watch = app
        .store
        .read(|db| {
            db.projects
                .iter()
                .find(|project| project.id == id)
                .map(|project| app.playback.watch(project))
        })
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Project not found."))?;
    if let Some(after) = cursor.after {
        let update = tokio::time::timeout(
            Duration::from_secs(20),
            watch.wait_for(|value| value.version != after),
        )
        .await;
        if !matches!(update, Ok(Ok(_))) {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
    }
    with_session(&app, &headers, |_| ())?;
    let view = app
        .store
        .read(|db| {
            db.projects
                .iter()
                .find(|project| project.id == id)
                .map(|project| watch.borrow().view(project, Instant::now()))
        })
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Project not found."))?;
    Ok(Json(view).into_response())
}

async fn browser_playback(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(command): Json<playback::Command>,
) -> Api<Json<playback::View>> {
    let view = app
        .store
        .read(|db| {
            let project = db
                .projects
                .iter()
                .find(|project| project.id == id)
                .context("Project not found.")?;
            app.playback.command(project, command)
        })
        .map_err(bad)?;
    Ok(Json(view))
}

async fn audio_mix(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    Query(version): Query<SourceVersion>,
) -> Api<Json<serde_json::Value>> {
    let project = preview_or_saved_project(&app, project, &version)
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Project not found."))?;
    let tracks = audio::mix(&app.store, &project).await.map_err(bad)?;
    Ok(Json(json!({"revision":project.revision,"tracks":tracks})))
}

#[derive(Deserialize)]
struct LibrarySearch {
    q: String,
    folder: Option<Uuid>,
    #[serde(default)]
    path: String,
}

async fn media_search(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    Query(input): Query<LibrarySearch>,
) -> Api<Json<serde_json::Value>> {
    if input.q.chars().count() > 300 {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Search can use up to 300 characters.",
        ));
    }
    let path = library::checked_relative(&input.path, true).map_err(bad)?;
    let sources = app
        .store
        .read(|db| {
            db.projects
                .iter()
                .find(|item| item.id == project)
                .map(|project| {
                    project
                        .sources
                        .iter()
                        .filter(|source| {
                            input.folder.is_none_or(|folder| {
                                project.library.scope(source.id, folder, &path)
                            })
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
        })
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Project not found."))?;
    let query = search::Query::library(&input.q);
    let result = tokio::task::spawn_blocking(move || search::find(&app.store, &sources, &query))
        .await
        .map_err(|_| {
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Media search is unavailable.",
            )
        })?;
    Ok(Json(result))
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum FolderRequest {
    Link {
        path: String,
    },
    Locate {
        folder: Uuid,
        path: String,
    },
    LocateSource {
        asset: Uuid,
        path: String,
    },
    Sync,
    Reload(ReloadRequest),
    CreateUpload {
        name: String,
    },
    CreateDevice {
        name: String,
        #[serde(default)]
        children: Vec<library::DeviceChild>,
    },
    BindDevice {
        folder: Uuid,
    },
    Remove {
        folder: Uuid,
        #[serde(default)]
        path: String,
    },
}

#[derive(Deserialize)]
struct ReloadRequest {
    #[serde(alias = "folder_id")]
    folder: Option<Uuid>,
    #[serde(alias = "asset_id")]
    asset: Option<Uuid>,
    asset_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    path: String,
}

impl ReloadRequest {
    fn target(self) -> Result<library::ReloadTarget> {
        use library::ReloadTarget;
        match (self.folder, self.asset, self.asset_ids) {
            (Some(folder), None, None) => Ok(ReloadTarget::Folder(folder, self.path)),
            (None, Some(asset), None) => Ok(ReloadTarget::Asset(asset)),
            (None, None, Some(assets)) => Ok(ReloadTarget::Assets(assets)),
            (None, None, None) => Ok(ReloadTarget::All),
            _ => bail!("Choose one folder or a selection of inbox sources to reload."),
        }
    }
}

#[derive(Deserialize)]
struct InboxQuery {
    #[serde(default)]
    files: bool,
}

async fn media_inbox(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Query(query): Query<InboxQuery>,
) -> Api<Json<serde_json::Value>> {
    if !app.store.read(|db| db.projects.iter().any(|p| p.id == id)) {
        return Err(failure(StatusCode::NOT_FOUND, "Project not found."));
    }
    let inventory = library::scan(&app.store).await.map_err(bad)?;
    let mut result = json!({"folders":inventory.tree()});
    if query.files {
        result["files"] = json!(inventory.file_paths().collect::<Vec<_>>());
    }
    Ok(Json(result))
}

async fn media_folders(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(input): Json<FolderRequest>,
) -> Api<Json<serde_json::Value>> {
    let result = async {
        Ok(match input {
            FolderRequest::Link { path } => {
                inbox_action(
                    &app,
                    id,
                    library::Action::Import {
                        paths: vec![path],
                        offer_match: true,
                    },
                    true,
                )
                .await?
            }
            FolderRequest::Locate { folder, path } => {
                inbox_action(&app, id, library::Action::Locate { folder, path }, true).await?
            }
            FolderRequest::LocateSource { asset, path } => {
                let _permit = app
                    .import_slot
                    .acquire()
                    .await
                    .context("The import service is unavailable.")?;
                let _inbox = app.inbox_state.lock().await;
                let inventory = library::scan(&app.store).await?;
                library::locate_source(&app.store, id, &inventory, asset, &path, MAX_IMPORT).await?
            }
            FolderRequest::Sync => inbox_action(&app, id, library::Action::Discover, true).await?,
            FolderRequest::Reload(request) => {
                inbox_action(&app, id, library::Action::Reload(request.target()?), true).await?
            }
            FolderRequest::CreateUpload { name } => {
                let name = title(&name)?;
                app.store.update(|db| {
                    let project = in_project(db, id)?;
                    let folder = project.library.upload_folder(name);
                    project.library_revision += 1;
                    Ok(folder.public())
                })?
            }
            FolderRequest::CreateDevice { name, children } => {
                let name = title(&name)?;
                app.store.update(|db| {
                    let project = in_project(db, id)?;
                    let result = project.library.create_device(name, children)?;
                    project.library_revision += 1;
                    Ok(result)
                })?
            }
            FolderRequest::BindDevice { folder } => app.store.update(|db| {
                let project = in_project(db, id)?;
                let result = project.library.bind_device(folder)?;
                project.library_revision += 1;
                Ok(result)
            })?,
            FolderRequest::Remove { folder, path } => {
                app.store
                    .update(|db| library::remove(in_project(db, id)?, folder, &path))?;
                json!({"removed":true})
            }
        })
    }
    .await;
    result.map(Json).map_err(bad)
}

fn device_binding(headers: &HeaderMap) -> Api<Uuid> {
    header_str(headers, "x-editroom-folder-binding")
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| failure(StatusCode::FORBIDDEN, "Reconnect this device folder."))
}

async fn device_folder_state(
    State(app): State<Shared>,
    Path((project, folder)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> Api<Json<serde_json::Value>> {
    let binding = device_binding(&headers)?;
    app.store
        .read(|db| {
            let project = db
                .projects
                .iter()
                .find(|item| item.id == project)
                .context("Project not found.")?;
            library::device_snapshot(project, folder, binding)
        })
        .map(Json)
        .map_err(bad)
}

#[derive(Deserialize)]
struct DeviceManifest {
    revision: u64,
    files: Vec<library::DeviceFile>,
    #[serde(default)]
    force: bool,
}

async fn sync_device_folder(
    State(app): State<Shared>,
    Path((project, folder)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(input): Json<DeviceManifest>,
) -> Api<Json<serde_json::Value>> {
    let binding = device_binding(&headers)?;
    app.store
        .update(|db| {
            let project = in_project(db, project)?;
            ensure!(
                project.library_revision == input.revision,
                "The media library changed. Scan again."
            );
            library::device_manifest(
                project,
                folder,
                binding,
                input.files,
                input.force,
                MAX_IMPORT as u64,
            )
        })
        .map(Json)
        .map_err(bad)
}

#[derive(Deserialize)]
struct Title {
    title: String,
}

fn title(title: &str) -> Result<String> {
    let title = title.trim();
    ensure!(
        !title.is_empty() && title.chars().count() <= 100 && !title.chars().any(char::is_control),
        "Enter a project or clip name up to 100 characters."
    );
    Ok(title.into())
}

async fn new_project(State(app): State<Shared>, Json(input): Json<Title>) -> Api<Json<Project>> {
    let project = Project::new(title(&input.title).map_err(bad)?);
    app.store
        .update(|db| db.add_project(project.clone()))
        .map_err(bad)?;
    Ok(Json(project))
}

#[derive(Deserialize)]
struct ProjectCopy {
    title: String,
    revision: u64,
    library_revision: u64,
}

async fn copy_project(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(input): Json<ProjectCopy>,
) -> Api<Json<serde_json::Value>> {
    let name = title(&input.title).map_err(bad)?;
    let copied = app
        .store
        .update(|db| {
            ensure!(
                !db.runs.iter().any(|run| run.project == id && run.pending()),
                "Wait for Director to finish before copying the project."
            );
            let project = in_project(db, id)?;
            ensure!(
                project.revision == input.revision
                    && project.library_revision == input.library_revision,
                "The project changed. Open Save As again to copy the current version."
            );
            let copy = project.copy_as(name);
            let copied = copy.id;
            db.add_project(copy)?;
            Ok(copied)
        })
        .map_err(bad)?;
    Ok(Json(json!({"id": copied})))
}

#[derive(Deserialize)]
struct Change {
    title: String,
    revision: u64,
    timeline: Vec<Clip>,
    soundtrack: Option<Uuid>,
}

fn in_project(db: &mut editor::Database, id: Uuid) -> Result<&mut Project> {
    db.projects
        .iter_mut()
        .find(|project| project.id == id)
        .context("Project not found.")
}

#[derive(Deserialize)]
struct RenderSettings {
    model: crate::render::Model,
}

async fn check_renderer(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
) -> Api<Json<serde_json::Value>> {
    let model = app
        .store
        .read(|db| {
            db.projects
                .iter()
                .find(|project| project.id == id)
                .map(|project| project.sequence.render_model)
        })
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Project not found."))?;
    let worker = app.worker.model(model);
    let (ready, message) = match tokio::time::timeout(Duration::from_secs(15), worker.check()).await
    {
        Ok(Ok(())) => (true, format!("Private GPU reachable · {}", worker.label())),
        Ok(Err(error)) => (false, error.to_string()),
        Err(_) => (
            false,
            "The private GPU did not respond in time. Check the connection and try again.".into(),
        ),
    };
    Ok(Json(
        json!({ "model": model, "ready": ready, "message": message }),
    ))
}

async fn set_renderer(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(input): Json<RenderSettings>,
) -> Api<StatusCode> {
    app.store
        .update(|db| {
            let project = in_project(db, id)?;
            project.sequence.render_model = input.model;
            project.revision += 1;
            Ok(())
        })
        .map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn load_lut(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Api<Json<editor::LookupTable>> {
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| failure(StatusCode::BAD_REQUEST, "Upload a text .cube LUT file."))?;
    crate::grade::Cube::parse(text).map_err(bad)?;
    let name =
        title(header_str(&headers, "x-editroom-title").unwrap_or("Custom look")).map_err(bad)?;
    let lookup = editor::LookupTable {
        id: Uuid::new_v4(),
        name,
    };
    let path = app.store.lut(lookup.id);
    editor::private_write(&path, &bytes).map_err(internal)?;
    let result = app.store.update(|db| {
        let project = in_project(db, id)?;
        ensure!(
            project.sequence.luts.len() < 30,
            "This project already has 30 custom looks."
        );
        project.sequence.luts.push(lookup.clone());
        project.revision += 1;
        Ok(())
    });
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(path).await;
        return Err(bad(error));
    }
    Ok(Json(lookup))
}

async fn grade_preview(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(raw): Json<serde_json::Value>,
) -> Api<Response> {
    let luts = app
        .store
        .read(|db| {
            db.projects
                .iter()
                .find(|project| project.id == id)
                .map(|project| project.sequence.luts.clone())
        })
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Project not found."))?;
    let grade = crate::grade::adjusted(&serde_json::Value::Null, &raw, &luts).map_err(bad)?;
    let path = grade["look"]
        .as_str()
        .and_then(crate::grade::look_id)
        .map(|id| app.store.lut(id));
    let slot = app
        .grade_slots
        .acquire()
        .await
        .map_err(|_| failure(StatusCode::SERVICE_UNAVAILABLE, "Grading is unavailable."))?;
    let bytes =
        tokio::task::spawn_blocking(move || crate::grade::lookup_bytes(&grade, path.as_deref()))
            .await
            .map_err(|error| internal(error.into()))?
            .map_err(bad)?;
    drop(slot);
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response())
}

async fn title_preview(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(title): Json<media::TitlePreview>,
) -> Api<Response> {
    overlay_preview(&app, id, move || title.render()).await
}

async fn caption_overlay_preview(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(caption): Json<crate::caption_export::Preview>,
) -> Api<Response> {
    overlay_preview(&app, id, move || caption.render()).await
}

async fn overlay_preview(
    app: &App,
    id: Uuid,
    render: impl FnOnce() -> Result<(Vec<u8>, u32, u32)> + Send + 'static,
) -> Api<Response> {
    if !app
        .store
        .read(|db| db.projects.iter().any(|project| project.id == id))
    {
        return Err(failure(StatusCode::NOT_FOUND, "Project not found."));
    }
    let slot = app
        .overlay_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| failure(StatusCode::TOO_MANY_REQUESTS, "Overlay previews are busy."))?;
    let (png, width, height) = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        render()
    })
    .await
    .map_err(|error| internal(error.into()))?
    .map_err(bad)?;
    Ok((
        [
            ("content-type", "image/png".to_owned()),
            ("x-editroom-overlay-size", format!("{width},{height}")),
        ],
        png,
    )
        .into_response())
}

async fn trim_preview(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(args): Json<serde_json::Value>,
) -> Api<Json<serde_json::Value>> {
    let mut preview = timeline_preview_project(&app, id, &args)?;
    Ok(Json(tools::trim_preview(&args, &mut preview).map_err(bad)?))
}

async fn retime_preview(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(args): Json<serde_json::Value>,
) -> Api<Json<serde_json::Value>> {
    let mut preview = timeline_preview_project(&app, id, &args)?;
    Ok(Json(
        tools::retime_preview(&args, &mut preview).map_err(bad)?,
    ))
}

fn timeline_preview_project(app: &App, id: Uuid, args: &serde_json::Value) -> Api<Project> {
    let preview = app
        .store
        .read(|db| {
            db.projects.iter().find(|p| p.id == id).map(|p| {
                let mut draft = Project::new(String::new());
                draft.timeline = p.timeline.clone();
                draft.sources = p.sources.clone();
                draft.sequence = p.sequence.clone();
                draft.soundtrack = p.soundtrack;
                draft.revision = p.revision;
                draft.library_revision = p.library_revision;
                draft
            })
        })
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Project not found."))?;
    if args["revision"].as_u64() != Some(preview.revision)
        || args["library_revision"]
            .as_u64()
            .is_some_and(|revision| revision != preview.library_revision)
    {
        return Err(failure(StatusCode::CONFLICT, "The timeline changed."));
    }
    Ok(preview)
}

async fn caption_preview(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Query(version): Query<SourceVersion>,
) -> Api<Json<serde_json::Value>> {
    let project = preview_or_saved_project(&app, id, &version)
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Project not found."))?;
    Ok(Json(json!(crate::captions::blocks(&project, &app.store))))
}

async fn update_project(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(input): Json<Change>,
) -> Api<Json<serde_json::Value>> {
    let name = title(&input.title).map_err(bad)?;
    let project = app
        .store
        .update(|db| {
            let project = in_project(db, id)?;
            ensure!(
                project.revision == input.revision,
                "The project changed. Reload the project before editing."
            );
            project.validate_timeline(&input.timeline, input.soundtrack)?;
            let before = project.snapshot();
            project.title = name;
            project.timeline = input.timeline;
            project.soundtrack = input.soundtrack;
            project.record(before);
            project.revision += 1;
            Ok(project.clone())
        })
        .map_err(bad)?;
    Ok(Json(project.public()))
}

#[derive(Deserialize)]
struct Ask {
    prompt: String,
    selected: Option<Uuid>,
    #[serde(default)]
    selected_clips: Vec<Uuid>,
    #[serde(default)]
    selected_media: library::MediaRemoval,
    duration: Option<f64>,
    #[serde(default)]
    quick: bool,
    playhead: Option<u32>,
}

async fn start_run(
    State(app): State<Shared>,
    Path(project_id): Path<Uuid>,
    headers: HeaderMap,
    Json(mut input): Json<Ask>,
) -> Api<(StatusCode, Json<Run>)> {
    let _edit = app
        .mcp_activity
        .channel(project_id)
        .director_turn()
        .map_err(bad)?;
    let provider = with_session(&app, &headers, Session::provider)?.ok_or_else(|| {
        failure(
            StatusCode::BAD_REQUEST,
            "Connect a ChatGPT or Claude API key first.",
        )
    })?;
    let prompt = input.prompt.trim().to_owned();
    if prompt.is_empty()
        || prompt.chars().count() > 3000
        || input.duration.is_some_and(|d| timing(d).is_none())
    {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Use up to 3,000 characters and a shot duration between 1 and 15 seconds.",
        ));
    }
    if input.playhead.is_some_and(|at| at > clock::MAX_TIME) {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "The playhead is outside the edit clock.",
        ));
    }
    let run = Run::new(
        project_id,
        vec![
            "Reading the project and planning the edit".into(),
            format!(
                "{}{}",
                provider.model,
                provider
                    .effort
                    .as_ref()
                    .map(|effort| format!(" · {effort} effort"))
                    .unwrap_or_default()
            ),
        ],
    );
    let project = app
        .store
        .update(|db| {
            ensure!(
                !db.runs.iter().any(Run::pending),
                "Director is finishing another request."
            );
            let project = in_project(db, project_id)?;
            ensure!(
                input.selected_clips.len() <= 500
                    && input
                        .selected
                        .into_iter()
                        .chain(input.selected_clips.iter().copied())
                        .all(|id| project.timeline.iter().any(|clip| clip.id == id)),
                "A selected clip is no longer on the timeline."
            );
            if !input.selected_media.is_empty() {
                library::plan_removal(project, input.selected_media.clone())?;
            }
            let tab = if input.quick {
                "video"
            } else {
                match provider.name {
                    Name::OpenAi => "chatgpt",
                    Name::Claude => "claude",
                    _ => "local",
                }
            };
            studio_history::remember_prompt(&mut project.prompt_history, &prompt, tab);
            project.remember_message(Message::new("you", prompt.clone()));
            let project = project.clone();
            db.runs.push(run.clone());
            if db.runs.len() > 120 {
                db.runs.remove(0);
            }
            Ok(project)
        })
        .map_err(bad)?;
    let run_id = run.id;
    let paused_at = app.playback.pause(&project);
    input.playhead.get_or_insert(paused_at);
    let preview = register_feed(&app, run_id);
    tokio::spawn(async move {
        let result = direct(&app, &provider, &project, input, run_id).await;
        let (status, message) = match result {
            Ok(reply) => (
                if reply.partial { "partial" } else { "done" },
                reply.message,
            ),
            Err(error) => ("failed", Message::new("director", error.to_string())),
        };
        if record_run_reply(&app, project.id, run_id, status, message)
            .await
            .is_err()
        {
            eprintln!("Could not record completion for Director run {run_id}");
        }
        finish_feed(&app, run_id, preview).await;
    });
    Ok((StatusCode::ACCEPTED, Json(run)))
}

async fn record_run_reply(
    app: &App,
    project: Uuid,
    id: Uuid,
    status: &str,
    mut message: Message,
) -> Result<()> {
    let discard = app.store.update(|db| {
        let run_index = db
            .runs
            .iter()
            .position(|run| run.id == id)
            .context("Director run not found.")?;
        let run = &db.runs[run_index];
        let preserve = status == "failed" && !run.stop_requested && !run.applied;
        let published = run
            .studio_turn
            .as_ref()
            .map(|turn| turn.asset_ids.clone())
            .unwrap_or_default();
        let reviewed = db.take_reviewed_shots(id);
        let saved = in_project(db, project)?;
        let mut kept = Vec::new();
        let mut discard = Vec::new();
        for source in reviewed {
            if published.contains(&source.id) || saved.source(source.id).is_some() {
                continue;
            }
            if preserve && app.store.media(source.storage_id(), source.video).is_file() {
                kept.push(source.id);
                saved.sources.push(source);
                saved.library_revision += 1;
            } else {
                discard.push(source);
            }
        }
        let run = &mut db.runs[run_index];
        if !kept.is_empty() {
            run.studio_turn_mut().asset_ids.extend(kept);
            message.text.push_str("\n\nReviewed footage is in Media.");
        }
        let message = run.finish(status, message);
        in_project(db, project)?.remember_message(message);
        db.discard_stopped_renders();
        Ok(discard)
    })?;
    for source in discard {
        media::discard_import(&app.store, &source).await;
    }
    Ok(())
}

async fn append_reviewed_shots(
    State(app): State<Shared>,
    Path((project, run)): Path<(Uuid, Uuid)>,
) -> Api<Json<serde_json::Value>> {
    let _edit = app
        .mcp_activity
        .channel(project)
        .director_turn()
        .map_err(bad)?;
    let clips = app
        .store
        .update(|db| {
            ensure!(
                !db.runs
                    .iter()
                    .any(|entry| entry.project == project && entry.pending()),
                "Wait for Director to finish editing this project."
            );
            let project = in_project(db, project)?;
            let turn = project
                .messages
                .iter()
                .rev()
                .filter_map(|message| message.studio.as_ref())
                .find(|turn| turn.id == run && turn.failed)
                .context("This project has no matching interrupted run.")?;
            let mut used: HashSet<_> = project.timeline.iter().map(|clip| clip.source).collect();
            let shots: Vec<_> = turn
                .asset_ids
                .iter()
                .copied()
                .filter(|id| used.insert(*id) && !project.library.offline(*id))
                .filter_map(|id| project.source(id))
                .filter(|source| source.video && !source.image)
                .map(crate::render_placement::ShotClips::new)
                .collect();
            if shots.is_empty() {
                return Ok(vec![]);
            }
            let before = project.snapshot();
            crate::render_placement::place(project, &shots, &Action::Append, None)?;
            project.record(before);
            project.revision += 1;
            Ok(shots.iter().map(|shot| shot.picture_id()).collect())
        })
        .map_err(bad)?;
    Ok(Json(json!({"clip_ids":clips})))
}

async fn start_auto_edit(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
) -> Api<(StatusCode, Json<Run>)> {
    let (run, _) = queue_auto_edit(app, project).map_err(bad)?;
    Ok((StatusCode::ACCEPTED, Json(run)))
}

fn queue_auto_edit(
    app: Shared,
    project_id: Uuid,
) -> Result<(
    Run,
    tokio::sync::oneshot::Receiver<Result<serde_json::Value>>,
)> {
    let _edit = app.mcp_activity.channel(project_id).director_turn()?;
    let run = Run::new(
        project_id,
        vec!["Auto Edit is looking through Media".into()],
    );
    let project = app.store.update(|db| {
        ensure!(
            !db.runs.iter().any(Run::pending),
            "Director is finishing another request."
        );
        let project = in_project(db, project_id)?;
        ensure!(
            project.sources.iter().any(|asset| asset.video
                && (asset.image || asset.duration >= clock::TICKS_PER_SECOND)
                && !project.library.offline(asset.id)),
            "Add usable video or photos to Media before running Auto Edit."
        );
        let saved = project.clone();
        db.runs.push(run.clone());
        if db.runs.len() > 120 {
            db.runs.remove(0);
        }
        Ok(saved)
    })?;
    let id = run.id;
    app.playback.pause(&project);
    let preview = register_feed(&app, id);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let planned = tokio::select! {
            result=run_auto_edit(&app,project,id,&preview)=>result,
            _=preview.closed()=>Err(anyhow::anyhow!("Auto Edit stopped. The draft was discarded.")),
        };
        let (status, note) = match &planned {
            Ok((_, note)) => ("done", note.clone()),
            Err(error) => ("failed", error.to_string()),
        };
        let finalized =
            record_run_reply(&app, project_id, id, status, Message::new("director", note)).await;
        let _ =
            sender.send(finalized.and(planned.map(|(project, _)| tools::project_json(&project))));
        finish_feed(&app, id, preview).await;
    });
    Ok((run, receiver))
}

async fn run_auto_edit(
    app: &App,
    project: Project,
    id: Uuid,
    preview: &Feed,
) -> Result<(Project, String)> {
    let plan = auto_edit::plan(&app.store, project.clone(), |activity| {
        event(app, id, activity)
    })
    .await?;
    apply_local_plan(app, project, id, preview, plan).await
}

async fn apply_local_plan(
    app: &App,
    project: Project,
    id: Uuid,
    preview: &Feed,
    plan: auto_edit::Plan,
) -> Result<(Project, String)> {
    let mut edits = Vec::new();
    let mut before = &project;
    for (batch, stage) in plan.stages.iter().enumerate() {
        check_stop(app, id)?;
        let changes = visible_edits(before, &stage.project, stage.action, batch);
        event(app, id, stage.activity);
        preview.push(&stage.project, changes.clone());
        edits.extend(
            changes
                .into_iter()
                .take(40usize.saturating_sub(edits.len())),
        );
        before = &stage.project;
        tokio::time::sleep(Duration::from_millis(660)).await;
    }
    let current = if let Some(stage) = plan.stages.into_iter().last() {
        app.store.update(|db| {
            record_director_commit(db, id)?;
            let current = in_project(db, project.id)?;
            ensure!(
                current.revision == project.revision
                    && current.library_revision == project.library_revision,
                "The cut or Media changed during Auto Edit. Run it again with the latest footage."
            );
            let prior = current.snapshot();
            let playhead = current.sequence.playhead;
            current.timeline = stage.project.timeline;
            current.soundtrack = stage.project.soundtrack;
            current.sequence = stage.project.sequence;
            current.sequence.playhead = playhead;
            current.sequence.playing = false;
            current.record(prior);
            current.revision += 1;
            let saved = current.clone();
            if let Some(run) = db.runs.iter_mut().find(|run| run.id == id) {
                run.studio_turn_mut().changes = edits.clone();
                run.edits = edits;
            }
            Ok(saved)
        })?
    } else {
        project
    };
    Ok((current, plan.summary))
}

async fn stop_run(State(app): State<Shared>, Path(project): Path<Uuid>) -> Api<StatusCode> {
    let id = app
        .store
        .update(|db| {
            let run = db
                .runs
                .iter_mut()
                .rev()
                .find(|run| run.project == project && run.pending())
                .context("This project has no active Director run.")?;
            if !run.applied && !run.stop_requested {
                run.stop_requested = true;
                run.events
                    .push("Stopping after the current operation.".into());
            }
            Ok((!run.applied).then_some(run.id))
        })
        .map_err(bad)?;
    if let Some(id) = id
        && let Some(feed) = feed(&app, id)
    {
        feed.close();
    }
    Ok(StatusCode::NO_CONTENT)
}

fn feed(app: &App, id: Uuid) -> Option<Arc<Feed>> {
    app.director_feeds
        .lock()
        .expect("director feeds")
        .get(&id)
        .cloned()
}

fn register_feed(app: &App, id: Uuid) -> Arc<Feed> {
    let preview = Arc::new(Feed::new());
    app.director_feeds
        .lock()
        .expect("director feeds")
        .insert(id, preview.clone());
    preview
}

async fn finish_feed(app: &App, id: Uuid, preview: Arc<Feed>) {
    preview.close();
    tokio::time::sleep(Duration::from_secs(3)).await;
    app.director_feeds
        .lock()
        .expect("director feeds")
        .remove(&id);
}

#[derive(Deserialize)]
struct DraftQuery {
    run: Uuid,
    #[serde(default)]
    after: u64,
    activity: Option<u64>,
}

async fn director_draft(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    Query(query): Query<DraftQuery>,
) -> Api<Json<serde_json::Value>> {
    let allowed = app.store.read(|db| {
        db.runs
            .iter()
            .any(|run| run.id == query.run && run.project == project && !run.stop_requested)
    });
    if allowed && let Some(feed) = feed(&app, query.run) {
        let mut view = feed.after(query.after, query.activity).await;
        let current = app.store.read(|db| {
            db.runs
                .iter()
                .find(|run| run.id == query.run && !run.stop_requested)
                .map(|run| view.get("activity_version").map(|_| run.events.clone()))
        });
        if let Some(activity) = current {
            if let Some(events) = activity {
                view["activity_events"] = json!(events);
            }
            return Ok(Json(view));
        }
    }
    Ok(Json(json!({"finished":true,"frames":[]})))
}

fn record_director_commit(db: &mut editor::Database, id: Uuid) -> Result<()> {
    let run = db
        .runs
        .iter_mut()
        .find(|run| run.id == id)
        .context("Director run no longer exists.")?;
    ensure!(
        run.pending() && !run.stop_requested,
        "Director stopped. The draft was discarded."
    );
    run.applied = true;
    Ok(())
}

fn check_stop(app: &App, id: Uuid) -> Result<()> {
    ensure!(
        !app.store
            .read(|db| db.runs.iter().any(|run| run.id == id && run.stop_requested)),
        "Director stopped. The draft was discarded."
    );
    Ok(())
}

fn event(app: &App, id: Uuid, text: impl Into<String>) {
    if app
        .store
        .update(|db| {
            if let Some(run) = db.runs.iter_mut().find(|run| run.id == id) {
                if run.events.len() >= studio_history::MAX_EVENTS {
                    run.events.remove(0);
                }
                run.events.push(text.into());
            }
            Ok(())
        })
        .is_err()
    {
        eprintln!("Could not save progress for Director run {id}");
    } else if let Some(feed) = feed(app, id) {
        feed.activity();
    }
}

fn settle(app: &App, id: Uuid, success: bool) {
    if app
        .store
        .update(|db| {
            db.settle(
                id,
                if success {
                    ChargeState::Spent
                } else {
                    ChargeState::Released
                },
            );
            Ok(())
        })
        .is_err()
    {
        eprintln!("Could not settle a render credit reservation {id}");
    }
}

async fn reconcile(app: Shared) {
    loop {
        let ids = app.store.read(|db| {
            db.charges
                .iter()
                .filter(|charge| {
                    charge.state == ChargeState::Reserved
                        && (charge.cancel_requested_at.is_some()
                            || editor::now().saturating_sub(charge.created) > 120)
                })
                .map(|charge| (charge.id, charge.created, charge.cancel_requested_at))
                .collect::<Vec<_>>()
        });
        for (id, created, cancel_requested_at) in ids {
            match app.worker.status(id).await {
                Ok(WorkerStatus::Ready { .. }) => settle(&app, id, true),
                Ok(WorkerStatus::Failed) => settle(&app, id, false),
                Ok(WorkerStatus::Unknown)
                    if cancel_requested_at
                        .is_some_and(|at| editor::now().saturating_sub(at) >= 90)
                        || editor::now().saturating_sub(created) >= 3600 =>
                {
                    settle(&app, id, false)
                }
                Ok(WorkerStatus::Queued | WorkerStatus::Rendering)
                    if cancel_requested_at.is_some() =>
                {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(4),
                        cancel_worker_job(&app, &app.worker, id),
                    )
                    .await;
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_secs(45)).await;
    }
}

async fn cancel_worker_job(app: &App, worker: &Worker, id: Uuid) {
    let queued = match worker.status(id).await {
        Ok(WorkerStatus::Ready { .. }) => {
            settle(app, id, true);
            return;
        }
        Ok(WorkerStatus::Failed) => {
            settle(app, id, false);
            return;
        }
        status => matches!(status, Ok(WorkerStatus::Queued)),
    };
    let dispatched = worker.cancel(id).await.unwrap_or(false);
    let mut status = worker.status(id).await;
    let removed = queued && dispatched && matches!(status, Ok(WorkerStatus::Unknown));
    if removed {
        // Queue and history are read separately. Check again if the job disappeared between them.
        tokio::time::sleep(Duration::from_millis(250)).await;
        status = worker.status(id).await;
    }
    match status {
        Ok(WorkerStatus::Ready { .. }) => settle(app, id, true),
        Ok(WorkerStatus::Failed) => settle(app, id, false),
        Ok(WorkerStatus::Unknown) if removed => settle(app, id, false),
        _ => {}
    }
}

struct WorkerShot<'a> {
    id: Uuid,
    title: &'a str,
    text: &'a str,
    visible: u32,
    rendered: u32,
    credits: u32,
    anchor: Option<String>,
    reference: Option<&'a std::path::Path>,
}

async fn run_worker_shot(
    app: &App,
    worker: &Worker,
    run: Uuid,
    shot: WorkerShot<'_>,
) -> Result<(String, String)> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let preview = feed(app, run).context("Director preview is unavailable.")?;
    let WorkerShot {
        id,
        title,
        text,
        visible,
        rendered,
        credits,
        anchor,
        reference,
    } = shot;
    let reference = if let Some(path) = reference {
        let bytes = tokio::fs::read(path).await?;
        ensure!(
            bytes.len() <= 512 * 1024,
            "The footage review reference is too large."
        );
        Some(STANDARD.encode(bytes))
    } else {
        None
    };
    app.store.update(|db| {
        db.reserve_render(
            id,
            run,
            title.into(),
            credits,
            visible,
            text.into(),
            reference,
        )
    })?;
    let render = Box::pin(async {
        if let Err(error) = worker.submit(id, text, rendered, anchor).await {
            settle(app, id, false);
            return Err(error);
        }
        worker
            .wait(
                id,
                |stage| event(app, run, format!("{stage} · {title}")),
                |done| settle(app, id, done),
            )
            .await
    });
    tokio::select! {
        result=render=>result,
        _=preview.closed()=>{
            app.store.update(|db| {
                if let Some(charge) = db.charges.iter_mut().find(|charge| charge.id == id && charge.state == ChargeState::Reserved) {
                    charge.cancel_requested_at = Some(editor::now());
                }
                Ok(())
            })?;
            let _ = tokio::time::timeout(Duration::from_secs(4),cancel_worker_job(app,worker,id)).await;
            bail!("Director stopped. The draft was discarded.");
        }
    }
}

struct DirectorReply {
    message: Message,
    partial: bool,
}

async fn recover_shot(
    State(app): State<Shared>,
    Path((project, job)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> Api<StatusCode> {
    let receipt = app
        .store
        .read(|db| db.render_receipt(project, job).cloned())
        .ok_or_else(|| {
            failure(
                StatusCode::NOT_FOUND,
                "This render cannot be recovered in this project.",
            )
        })?;
    let provider = with_session(&app, &headers, Session::provider)?
        .filter(|provider| provider.name != Name::Local || !crate::provider::builtin_local())
        .ok_or_else(|| {
            failure(
                StatusCode::BAD_REQUEST,
                "Connect ChatGPT or Claude to review the recovered footage.",
            )
        })?;
    {
        let mut active = app.recovering_render.lock().expect("render recovery");
        if active.is_some() {
            return Err(failure(
                StatusCode::CONFLICT,
                "Wait for the current footage recovery to finish.",
            ));
        }
        *active = Some(job);
    }
    tokio::spawn(async move {
        if let Err(error) = Box::pin(recover_worker_output(&app, &provider, job, &receipt)).await {
            let issue = error.to_string().chars().take(400).collect();
            if app
                .store
                .update(|db| {
                    db.record_render_issue(job, issue);
                    Ok(())
                })
                .is_err()
            {
                eprintln!("Could not save render recovery outcome {job}");
            }
        }
        *app.recovering_render.lock().expect("render recovery") = None;
    });
    Ok(StatusCode::ACCEPTED)
}

async fn recover_worker_output(
    app: &App,
    provider: &Provider,
    job: Uuid,
    receipt: &RenderReceipt,
) -> Result<()> {
    let (filename, folder) = match app
        .worker
        .status(job)
        .await
        .map_err(|_| anyhow::anyhow!("The private GPU worker could not be reached."))?
    {
        WorkerStatus::Ready {
            filename,
            subfolder,
        } => (filename, subfolder),
        WorkerStatus::Queued | WorkerStatus::Rendering => {
            bail!("The original GPU job is still in progress.")
        }
        WorkerStatus::Unknown => bail!("The worker has no result for the original job yet."),
        WorkerStatus::Failed => {
            settle(app, job, false);
            bail!("The original GPU job did not produce a video.");
        }
    };
    settle(app, job, true);
    let work = media::temp(&app.store).await?;
    let outcome = Box::pin(review_recovered_file(
        app, provider, job, receipt, filename, folder, &work,
    ))
    .await;
    if !app.store.read(|db| db.has_reviewed_shot(receipt.run, job)) {
        let _ = tokio::fs::remove_dir_all(work).await;
    }
    outcome
}

async fn review_recovered_file(
    app: &App,
    provider: &Provider,
    job: Uuid,
    receipt: &RenderReceipt,
    filename: String,
    folder: String,
    work: &std::path::Path,
) -> Result<()> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let raw = work.join("recovered-raw.mp4");
    app.worker.download(filename, folder, &raw, |_| {}).await?;
    let video = work.join(format!("{job}-visible.mp4"));
    let meta = media::cut_render(&raw, &video, receipt.visible)
        .await
        .context("The recovered video could not be read.")?;
    let reference = if let Some(encoded) = &receipt.reference {
        let image = work.join("reference.jpg");
        tokio::fs::write(
            &image,
            STANDARD
                .decode(encoded)
                .context("The saved footage reference is unreadable.")?,
        )
        .await?;
        Some(image)
    } else {
        None
    };
    let review = Box::pin(review_rendered_shot(
        app,
        provider,
        &receipt.direction,
        &video,
        receipt.visible,
        reference.as_deref(),
        work,
    ))
    .await?;
    ensure!(
        review.accepted,
        "The planner rejected the recovered footage. {}",
        review.note
    );
    let source = Source {
        storage: None,
        origin_stamp: None,
        device_digest: None,
        id: job,
        title: receipt.title.clone(),
        duration: receipt.visible * clock::MODEL_FRAME,
        video: true,
        audio: meta.audio,
        image: false,
        notes: review.note,
    };
    install_reviewed_shot(&app.store, receipt.run, &source, work).await?;
    app.store
        .update(|db| db.publish_recovered_shot(receipt.project, source))
}

async fn review_rendered_shot(
    app: &App,
    provider: &Provider,
    direction: &str,
    video: &std::path::Path,
    visible: u32,
    reference: Option<&std::path::Path>,
    work: &std::path::Path,
) -> Result<crate::provider::Review> {
    let mut samples = reference
        .map(|file| vec![file.to_owned()])
        .unwrap_or_default();
    for (index, frame) in [0, visible / 2, visible - 1].into_iter().enumerate() {
        let path = work.join(format!("review-{index}.jpg"));
        media::extract(video, frame, &path, true)
            .await
            .context("Could not read frames for footage review.")?;
        samples.push(path);
    }
    let samples = samples.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    provider
        .review(&app.client, direction, &samples, reference.is_some())
        .await
}

impl From<Message> for DirectorReply {
    fn from(message: Message) -> Self {
        Self {
            message,
            partial: false,
        }
    }
}

async fn direct(
    app: &Shared,
    provider: &Provider,
    project: &Project,
    input: Ask,
    run: Uuid,
) -> Result<DirectorReply> {
    if provider.name == Name::Local && crate::provider::builtin_local() {
        if input.quick {
            return Ok(Message::new(
                "director",
                "Choose ChatGPT or Claude to plan a Quick Clips render. Local edits footage already in Media.".into(),
            ).into());
        }
        let preview = feed(app, run).context("Director preview is unavailable.")?;
        let at = input
            .playhead
            .unwrap_or_else(|| app.playback.view(project).at);
        let mut selection = input.selected_clips;
        if let Some(id) = input.selected
            && !selection.contains(&id)
        {
            selection.push(id);
        }
        let work = media::temp(&app.store).await?;
        let result = tokio::select! {
            result=async {
                let plan=auto_edit::from_prompt(&app.store,project.clone(),&input.prompt,at,&selection,&work,
                    |activity|event(app,run,activity)).await?;
                let (_,summary)=apply_local_plan(app,project.clone(),run,&preview,plan).await?;
                Ok(Message::new("director",summary))
            }=>result,
            _=preview.closed()=>Err(anyhow::anyhow!("Local stopped. The draft was discarded.")),
        };
        let _ = tokio::fs::remove_dir_all(work).await;
        return result.map(Into::into);
    }
    if !input.quick
        && (provider.name != Name::Fixture || env::var_os("EDITROOM_FIXTURE_TOOLS").is_some())
    {
        return agent_direct(app, provider, project, input, run).await;
    }
    check_stop(app, run)?;
    let preview = feed(app, run).context("Director preview is unavailable.")?;
    let mut plan = tokio::select! {
        plan=provider.plan(
            &app.client,
            &input.prompt,
            project,
            input.selected,
            input.duration,
        )=>plan?,
        _=preview.closed()=>bail!("Director stopped. The draft was discarded."),
    };
    if let Some(duration) = input.duration {
        plan.set_shot_duration(duration)?;
    }
    match plan.action {
        Action::Reply => return Ok(Message::new("director", plan.message).into()),
        Action::Edit => {
            let mut draft = project.clone();
            draft.timeline = plan.timeline;
            draft.validate_timeline(&draft.timeline, draft.soundtrack)?;
            let edits = visible_edits(project, &draft, "edit", 0)
                .into_iter()
                .take(40)
                .collect::<Vec<_>>();
            preview.push(&draft, edits.clone());
            app.store.update(|db| {
                record_director_commit(db, run)?;
                let current = in_project(db, project.id)?;
                ensure!(
                    current.revision == project.revision,
                    "The project changed while Director was preparing the edit."
                );
                current.record(current.snapshot());
                current.timeline = draft.timeline;
                current.revision += 1;
                if let Some(run) = db.runs.iter_mut().find(|item| item.id == run) {
                    run.studio_turn_mut().changes = edits.clone();
                    run.edits = edits;
                }
                Ok(())
            })?;
            event(app, run, "Timeline updated");
            return Ok(Message::new("director", plan.message).into());
        }
        _ => {}
    }
    app.store.update(|db| {
        let record = db
            .runs
            .iter_mut()
            .find(|item| item.id == run)
            .context("Director run not found.")?;
        record.studio_turn_mut().plan = Some((&plan).into());
        Ok(())
    })?;
    let first = match plan.action {
        Action::Continue => input
            .selected
            .and_then(|id| project.timeline.iter().find(|clip| clip.id == id))
            .or_else(|| {
                project
                    .timeline
                    .iter()
                    .filter(|clip| clip.kind == editor::ClipKind::Video)
                    .max_by_key(|clip| project.placement(clip.id).unwrap_or(0) + clip.duration())
            }),
        Action::ReplaceSelected => Some(
            input
                .selected
                .and_then(|id| project.timeline.iter().find(|clip| clip.id == id))
                .context("Select a clip to replace.")?,
        ),
        _ => None,
    };
    if matches!(plan.action, Action::Continue) && first.is_none() {
        bail!("Add footage before asking Director to continue a clip.");
    }
    if let Some(clip) = first {
        crate::render_placement::check_target(project, clip, &plan.action)?;
    }
    let estimated: u32 = plan
        .shots
        .iter()
        .map(|shot| timing(shot.duration_seconds).expect("validated plan").2)
        .sum();
    let available = app.store.read(|db| db.available());
    ensure!(
        available >= estimated,
        "This plan needs {estimated} credits. {available} are available."
    );
    event(
        app,
        run,
        format!(
            "Planned {} shots · {estimated} credits before any retakes",
            plan.shots.len()
        ),
    );
    app.worker
        .model(project.sequence.render_model)
        .check()
        .await?;
    let dir = media::temp(&app.store).await?;
    let outcome = Box::pin(render_plan(app, provider, project, &plan, first, run, &dir)).await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    outcome.map(Into::into)
}

async fn render_plan(
    app: &App,
    provider: &Provider,
    project: &Project,
    plan: &crate::provider::Plan,
    first: Option<&Clip>,
    run: Uuid,
    dir: &std::path::Path,
) -> Result<Message> {
    let worker = app.worker.model(project.sequence.render_model);
    let mut anchor = if matches!(plan.action, Action::Continue) {
        let clip = first.expect("validated continuation");
        Some(Box::pin(media::continuation_frame(&app.store, project, clip, dir)).await?)
    } else {
        None
    };
    let mut clips = vec![];
    let preview = feed(app, run).context("Director preview is unavailable.")?;
    let mut draft = project.clone();
    let mut edits = Vec::new();
    let mut render_format = None;
    let use_render_format = project.timeline.is_empty()
        && project.sources.is_empty()
        && project.sequence.offers_source_format();
    for (index, shot) in plan.shots.iter().enumerate() {
        check_stop(app, run)?;
        let (visible, rendered, credits) =
            timing(shot.duration_seconds).expect("validated duration");
        let mut correction = String::new();
        for take in 1..=2 {
            check_stop(app, run)?;
            let id = Uuid::new_v4();
            let worker_anchor = if let Some((image, _)) = &anchor {
                event(
                    app,
                    run,
                    format!("Starting {} from the last visible frame", shot.title),
                );
                Some(worker.upload(id, image).await.map_err(|_| {
                    anyhow::anyhow!("Could not transfer the last visible frame to ComfyUI.")
                })?)
            } else {
                None
            };
            let mut direction = format!("{}\n{}", plan.continuity, shot.direction);
            if worker_anchor.is_some() {
                direction.push_str("\nBegin exactly from the supplied frame. Continue the action immediately in the same physical direction. Preserve the established subject and scene.");
            }
            if !correction.is_empty() {
                direction.push_str(&format!("\nRetake direction: {correction}"));
            }
            check_stop(app, run)?;
            event(
                app,
                run,
                format!(
                    "Submitting {} · {:.2}s · take {take}",
                    shot.title,
                    f64::from(visible) / 24.0
                ),
            );
            let (filename, subfolder) = run_worker_shot(
                app,
                &worker,
                run,
                WorkerShot {
                    id,
                    title: &shot.title,
                    text: &direction,
                    visible,
                    rendered,
                    credits,
                    anchor: worker_anchor,
                    reference: anchor.as_ref().map(|(_, thumb)| thumb.as_path()),
                },
            )
            .await?;
            check_stop(app, run)?;
            let raw = dir.join(format!("{id}-raw.mp4"));
            tokio::select! {
                downloaded=worker.download(filename, subfolder, &raw, |stage| {
                    event(app, run, format!("{stage} · {}", shot.title));
                })=>downloaded?,
                _=preview.closed()=>bail!("Director stopped. The draft was discarded."),
            }
            let edited = dir.join(format!("{id}-visible.mp4"));
            let meta = media::cut_render(&raw, &edited, visible).await?;
            event(
                app,
                run,
                format!("Checking action and continuity · {}", shot.title),
            );
            let review = tokio::select! {
                review=Box::pin(review_rendered_shot(
                    app,provider,&direction,&edited,visible,anchor.as_ref().map(|(_,thumb)|thumb.as_path()),dir,
                )
                )=>review?,
                _=preview.closed()=>bail!("Director stopped. The draft was discarded."),
            };
            if !review.accepted {
                app.store.update(|db| {
                    db.finish_render_receipt(id);
                    Ok(())
                })?;
                correction = review.correction;
                event(
                    app,
                    run,
                    format!("Rejected {} · {}", shot.title, review.note),
                );
                if take == 2 {
                    bail!(
                        "Director rejected both takes of {}. Accepted shots are in the media library.",
                        shot.title
                    );
                }
                continue;
            }
            check_stop(app, run)?;
            let source = Source {
                storage: None,
                origin_stamp: None,
                device_digest: None,
                id,
                title: shot.title.clone(),
                duration: visible * clock::MODEL_FRAME,
                video: true,
                audio: meta.audio,
                image: false,
                notes: review.note.clone(),
            };
            let path = install_reviewed_shot(&app.store, run, &source, dir).await?;
            let _ = media::extract(&path, 0, &thumb_path(&app.store, id), true).await;
            if clips.is_empty()
                && let Ok(mut format) = media::picture_format(&app.store, &source).await
            {
                format.fps = f64::from(crate::FPS);
                render_format = Some(format);
                if use_render_format {
                    draft.sequence.set_format(format);
                }
            }
            app.store.update(|db| {
                let project = in_project(db, project.id)?;
                project.sources.push(source.clone());
                project.library_revision += 1;
                let record = db
                    .runs
                    .iter_mut()
                    .find(|item| item.id == run)
                    .context("Director run not found.")?;
                record.studio_turn_mut().asset_ids.push(source.id);
                Ok(())
            })?;
            event(
                app,
                run,
                format!("Accepted {} · {}", shot.title, review.note),
            );
            if index + 1 < plan.shots.len() {
                let image = dir.join(format!("{id}-last.png"));
                let thumbnail = dir.join(format!("{id}-last.jpg"));
                media::extract_at(&path, visible * clock::MODEL_FRAME - 1, &image, false).await?;
                media::extract(&image, 0, &thumbnail, true).await?;
                anchor = Some((image, thumbnail));
            }
            clips.push(crate::render_placement::ShotClips::new(&source));
            let before = draft.clone();
            draft.sources.push(source);
            draft.timeline.clone_from(&project.timeline);
            draft.sequence.tracks.clone_from(&project.sequence.tracks);
            crate::render_placement::place(
                &mut draft,
                &clips,
                &plan.action,
                first.map(|clip| clip.id),
            )?;
            let changes = visible_edits(&before, &draft, "add_clip", edits.len())
                .into_iter()
                .take(40)
                .collect::<Vec<_>>();
            preview.push(&draft, changes.clone());
            edits.extend(changes.into_iter().take(40 - edits.len()));
            break;
        }
    }
    check_stop(app, run)?;
    let count = clips.len();
    app.store.update(|db| {
        record_director_commit(db, run)?;
        let project_before = in_project(db, project.id)?;
        if let Some(clip) = first {
            crate::render_placement::verify_target(project, project_before, clip, &plan.action)?;
        }
        let prior = project_before.snapshot();
        let changed_while_rendering =
            (project_before.revision != project.revision).then(|| project_before.clone());
        if let Some(format) = render_format
            && project_before.sequence.offers_source_format()
            && project_before.timeline.is_empty()
            && project_before
                .sources
                .iter()
                .all(|source| clips.iter().any(|clip| clip.asset() == source.id))
        {
            project_before.sequence.set_format(format);
        }
        crate::render_placement::place(
            project_before,
            &clips,
            &plan.action,
            first.map(|clip| clip.id),
        )?;
        project_before.record(prior);
        project_before.revision += 1;
        if let Some(before) = changed_while_rendering {
            edits = visible_edits(&before, project_before, "add_clip", 0)
                .into_iter()
                .take(40)
                .collect();
        }
        if let Some(run) = db.runs.iter_mut().find(|item| item.id == run) {
            run.studio_turn_mut().changes = edits.clone();
            run.edits = edits;
        }
        Ok(())
    })?;
    event(
        app,
        run,
        format!("Added {count} reviewed shots to the timeline"),
    );
    Ok(Message::new(
        "director",
        format!(
            "{} {} reviewed {} on the timeline.",
            plan.message,
            count,
            if count == 1 { "shot is" } else { "shots are" }
        ),
    ))
}

#[derive(Deserialize)]
struct UploadDestination {
    replace: Option<Uuid>,
    folder: Option<Uuid>,
    path: Option<String>,
    #[serde(default)]
    offer_match: bool,
    #[serde(default)]
    locate: bool,
}

async fn import(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    Query(destination): Query<UploadDestination>,
    headers: HeaderMap,
    request: Request,
) -> Api<Json<serde_json::Value>> {
    let name = header_str(&headers, "x-editroom-title").unwrap_or("Imported media");
    upload_media(&app, project, destination, name, None, request).await
}

#[derive(Deserialize)]
struct AgentUploadFolder {
    name: String,
}

struct AgentUploadSession {
    owner: mcp_activity::Owner,
    id: Uuid,
}

impl AgentUploadSession {
    fn authenticate(app: &App, project: Uuid, headers: &HeaderMap) -> Api<Self> {
        let owner = attached_owner(app, headers)
            .ok_or_else(|| failure(StatusCode::UNAUTHORIZED, "Use an attached-agent key."))?;
        let id = header_str(headers, "mcp-session-id")
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| {
                failure(
                    StatusCode::FORBIDDEN,
                    "Initialize the agent for this project.",
                )
            })?;
        let session = Self { owner, id };
        drop(session.check(app, project).map_err(|_| {
            failure(
                StatusCode::FORBIDDEN,
                "Initialize the agent for this project.",
            )
        })?);
        Ok(session)
    }

    fn check<'a>(
        &self,
        app: &'a App,
        project: Uuid,
    ) -> Result<Option<std::sync::MutexGuard<'a, crate::attached_agents::Access>>> {
        let access = confirm_attached_access(app, &self.owner)?;
        ensure!(
            app.mcp_activity
                .client(project, &self.owner, self.id)
                .is_some(),
            "The attached-agent session has ended."
        );
        Ok(access)
    }
}

async fn create_agent_upload_folder(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<AgentUploadFolder>,
) -> Api<Json<serde_json::Value>> {
    let session = AgentUploadSession::authenticate(&app, project, &headers)?;
    let name = title(&input.name).map_err(bad)?;
    app.store
        .update(|db| {
            let _access = session.check(&app, project)?;
            let project = in_project(db, project)?;
            let folder = project.library.upload_folder(name);
            project.library_revision += 1;
            Ok(folder.public())
        })
        .map(Json)
        .map_err(bad)
}

#[derive(Deserialize)]
struct AgentMedia {
    title: String,
    folder: Option<Uuid>,
    path: Option<String>,
}

async fn agent_upload(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    Query(input): Query<AgentMedia>,
    request: Request,
) -> Api<Json<serde_json::Value>> {
    let session = AgentUploadSession::authenticate(&app, project, request.headers())?;
    let destination = UploadDestination {
        folder: input.folder,
        path: input.path,
        replace: None,
        offer_match: false,
        locate: false,
    };
    upload_media(
        &app,
        project,
        destination,
        &input.title,
        Some(session),
        request,
    )
    .await
}

async fn upload_media(
    app: &App,
    project: Uuid,
    destination: UploadDestination,
    name: &str,
    agent: Option<AgentUploadSession>,
    request: Request,
) -> Api<Json<serde_json::Value>> {
    let size = upload_length(request.headers())?;
    let title = title(name).map_err(bad)?;
    let describe = |project: &Project, source: &Source| {
        if agent.is_none() {
            return project.library.source_json(source);
        }
        let mut media = tools::asset_json(source);
        if let Some((folder, path)) = project.library.locate(source.id) {
            media["folder_id"] = json!(folder.id);
            media["relative_path"] = json!(path);
        }
        media
    };
    let _permit = app.import_slot.acquire().await.map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The import service is unavailable.",
        )
    })?;
    let upload = app
        .store
        .read(|db| -> Result<_> {
            let project = db
                .projects
                .iter()
                .find(|item| item.id == project)
                .context("Project not found.")?;
            let place = match (destination.folder, destination.path.as_deref()) {
                (Some(folder), Some(path)) => Some(project.library.upload_path(folder, path)?),
                (None, None) => None,
                _ => bail!("Provide both folder and file path."),
            };
            let replacing = if let Some(id) = destination.replace {
                ensure!(place.is_none(), "Choose one source to replace.");
                ensure!(
                    !destination.locate || project.library.missing.contains_key(&id),
                    "That source has already been located."
                );
                ensure!(
                    !project.library.can_reload(id),
                    "Use Reload from Disk for an inbox-linked source."
                );
                Some(
                    project
                        .source(id)
                        .cloned()
                        .context("Source not found in this project.")?,
                )
            } else {
                ensure!(!destination.locate, "Choose a missing source to locate.");
                None
            };
            let previous = place
                .as_ref()
                .and_then(|place| project.library.existing(place))
                .and_then(|id| project.source(id))
                .map(|source| describe(project, source));
            Ok((
                place,
                previous,
                replacing,
                project.sequence.offers_source_format(),
            ))
        })
        .map_err(bad)?;
    let (place, previous, replacing, unlocked_format) = upload;
    if let Some(previous) = previous {
        return Ok(Json(previous));
    }
    let dir = media::temp(&app.store).await.map_err(internal)?;
    let result = async {
        let path = dir.join("input.media");
        receive_upload(request, &path, size, None).await?;
        let title = place
            .as_ref()
            .map(library::EntryPath::title)
            .unwrap_or(title);
        let source = media::import(&app.store, &path, title).await.map_err(bad)?;
        if replacing
            .as_ref()
            .is_some_and(|old| !old.same_kind(&source))
        {
            media::discard_import(&app.store, &source).await;
            return Err(bad(anyhow::anyhow!(
                "Choose the same media kind for this source."
            )));
        }
        if source.video {
            let _ = media::extract(
                &app.store.media(source.id, true),
                0,
                &thumb_path(&app.store, source.id),
                true,
            )
            .await;
        }
        let offer = if destination.offer_match && unlocked_format && replacing.is_none() {
            media::sequence_offer(&app.store, &source).await
        } else {
            None
        };
        let committed = app.store.update(|db| {
            let _access = match agent.as_ref() {
                Some(session) => session.check(app, project)?,
                None => None,
            };
            let project = in_project(db, project)?;
            if let Some(old) = &replacing {
                ensure!(
                    project
                        .source(old.id)
                        .is_some_and(|current| current.storage_id() == old.storage_id()),
                    "The source changed during the upload. Try the replacement again."
                );
                project.replace_sources(vec![(old.id, source.clone())]);
                return Ok(describe(project, project.source(old.id).unwrap()));
            }
            if let Some(place) = place {
                let folder = destination.folder.expect("folder upload");
                project
                    .library
                    .upload_path(folder, destination.path.as_deref().unwrap())?;
                project.library.add_upload(place, source.id);
            }
            project.sources.push(source.clone());
            project.library_revision += 1;
            project.offer_sequence_format(offer);
            Ok(describe(project, &source))
        });
        let result = match committed {
            Ok(result) => result,
            Err(error) => {
                media::discard_import(&app.store, &source).await;
                return Err(bad(error));
            }
        };
        Ok(Json(result))
    }
    .await;
    let _ = tokio::fs::remove_dir_all(dir).await;
    result
}

#[derive(Deserialize)]
struct SequenceChoice {
    asset: Uuid,
    version: Uuid,
    match_source: bool,
}

async fn decide_sequence_offer(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Json(input): Json<SequenceChoice>,
) -> Api<StatusCode> {
    app.store
        .update(|db| {
            let project = in_project(db, id)?;
            let Some(offer) = project
                .pending_sequence_offer()
                .filter(|offer| offer.asset == input.asset && offer.version == input.version)
            else {
                return Ok(());
            };
            let format = offer.format;
            if input.match_source && format != project.sequence.format() {
                let before = project.snapshot();
                project.sequence.set_format(format);
                project.record(before);
            }
            project.lock_sequence_settings();
            project.revision += 1;
            Ok(())
        })
        .map_err(bad)?;
    Ok(StatusCode::NO_CONTENT)
}

fn upload_length(headers: &HeaderMap) -> Api<usize> {
    let size = header_str(&headers, "content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if size == 0 || size > MAX_IMPORT {
        return Err(failure(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Use a video or audio file up to 256 MB.",
        ));
    }
    Ok(size)
}

async fn open_room(
    State(app): State<Shared>,
    mut parts: Multipart,
) -> Api<Json<serde_json::Value>> {
    let _permit = app.import_slot.acquire().await.map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The import service is unavailable.",
        )
    })?;
    let dir = media::temp(&app.store).await.map_err(internal)?;
    let result = async {
        let mut archive = None;
        let mut document = None;
        let mut media = HashMap::new();
        let mut looks = HashMap::new();
        let mut total = 0_u64;
        let mut fields = 0;
        loop {
            let part = parts
                .next_field()
                .await
                .map_err(|error| failure(error.status(), "The project upload was interrupted."))?;
            let Some(mut part) = part else { break };
            fields += 1;
            if fields > 1200 {
                return Err(failure(
                    StatusCode::BAD_REQUEST,
                    "The project has too many attachments.",
                ));
            }
            let path = dir.join(format!("part-{fields}"));
            let limit = match part.name().unwrap_or_default() {
                "bundle" if archive.is_none() => {
                    archive = Some(path.clone());
                    room::MAX_BUNDLE
                }
                "document" if document.is_none() => {
                    let name =
                        std::path::Path::new(part.file_name().unwrap_or("Editroom project.room"))
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Editroom project");
                    document = Some((path.clone(), name.to_string()));
                    room::MAX_DOCUMENT
                }
                item if item.starts_with("asset:") => {
                    let id = item["asset:".len()..].parse::<Uuid>().map_err(|_| {
                        failure(
                            StatusCode::BAD_REQUEST,
                            "An attachment has an invalid media ID.",
                        )
                    })?;
                    if media.insert(id, path.clone()).is_some() {
                        return Err(failure(
                            StatusCode::BAD_REQUEST,
                            "Two attachments were supplied for one media file.",
                        ));
                    }
                    room::MAX_BUNDLE
                }
                item if item.starts_with("look:") && item.len() <= 1600 => {
                    if looks
                        .insert(item["look:".len()..].to_owned(), path.clone())
                        .is_some()
                    {
                        return Err(failure(
                            StatusCode::BAD_REQUEST,
                            "Two attachments were supplied for one look.",
                        ));
                    }
                    14 * 1024 * 1024
                }
                _ => {
                    return Err(failure(
                        StatusCode::BAD_REQUEST,
                        "Unexpected item in the project upload.",
                    ));
                }
            };
            let mut file = tokio::fs::File::create(&path)
                .await
                .map_err(|error| internal(error.into()))?;
            let mut bytes = 0_u64;
            while let Some(data) = part
                .chunk()
                .await
                .map_err(|error| failure(error.status(), "The project upload was interrupted."))?
            {
                bytes += data.len() as u64;
                total += data.len() as u64;
                if bytes > limit || total > room::MAX_BUNDLE {
                    return Err(failure(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "The project exceeds the upload limit.",
                    ));
                }
                file.write_all(&data)
                    .await
                    .map_err(|error| internal(error.into()))?;
            }
            file.flush().await.map_err(|error| internal(error.into()))?;
        }
        let staged = dir.join("files");
        let (path, name, access) = match (archive, document) {
            (Some(archive), None) if media.is_empty() && looks.is_empty() => {
                let unpacked = staged.clone();
                let file = tokio::task::spawn_blocking(move || room::unpack(&archive, &unpacked))
                    .await
                    .map_err(|error| internal(error.into()))?
                    .map_err(bad)?;
                let name = file
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or("Editroom project")
                    .to_owned();
                (file.clone(), name, Some(file))
            }
            (None, Some((path, name))) => (path, name, None),
            _ => {
                return Err(failure(
                    StatusCode::BAD_REQUEST,
                    "Choose one .room or .cutplay document or one project ZIP.",
                ));
            }
        };
        let doc = tokio::fs::read(&path)
            .await
            .map_err(|error| internal(error.into()))?;
        let doc = serde_json::from_slice(&doc).map_err(|_| {
            failure(
                StatusCode::BAD_REQUEST,
                "The document is not a valid Editroom project.",
            )
        })?;
        let files = match access.as_deref() {
            Some(document) => room::RoomFiles::Archive {
                root: &staged,
                document,
            },
            None => room::RoomFiles::Selected {
                media: &media,
                looks: &looks,
            },
        };
        let id = room::import_project(&app.store, &doc, &name, files)
            .await
            .map_err(bad)?;
        Ok(Json(json!({"id":id,"title":name})))
    }
    .await;
    let _ = tokio::fs::remove_dir_all(dir).await;
    result
}

#[derive(Deserialize)]
struct RoomDownload {
    name: Option<String>,
}

async fn save_room(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Query(download): Query<RoomDownload>,
) -> Api<Response> {
    let mut project = app
        .store
        .read(|db| db.projects.iter().find(|project| project.id == id).cloned())
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Project not found."))?;
    if let Some(name) = download.name {
        project.title = title(&name).map_err(bad)?;
    }
    let _permit = app
        .export_slot
        .try_acquire()
        .map_err(|_| failure(StatusCode::CONFLICT, "Another export is already running."))?;
    let dir = media::temp(&app.store).await.map_err(internal)?;
    let result=async {
        let path=room::bundle(&app.store,&project,&dir).await.map_err(bad)?;
        let file=tokio::fs::File::open(path).await.map_err(|error|internal(error.into()))?;
        let size=file.metadata().await.map_err(|error|internal(error.into()))?.len();
        let name=room::archive_name(&project);
        let encoded=encoded_filename(&name);
        Ok((
            [(header::CONTENT_TYPE, "application/zip".to_owned()),(header::CONTENT_LENGTH,size.to_string()),
             (header::CONTENT_DISPOSITION,format!("attachment; filename=\"Editroom project.zip\"; filename*=UTF-8''{encoded}.zip"))],
            Body::from_stream(tokio_util::io::ReaderStream::new(file))
        ).into_response())
    }.await;
    let _ = tokio::fs::remove_dir_all(dir).await;
    result
}

async fn receive_upload(
    request: Request,
    path: &std::path::Path,
    size: usize,
    checksum: Option<&str>,
) -> Api<()> {
    let mut output = tokio::fs::File::create(path)
        .await
        .map_err(|e| internal(e.into()))?;
    let mut digest = checksum.map(|_| ring::digest::Context::new(&ring::digest::SHA256));
    let mut body = request.into_body();
    let mut length = 0;
    while let Some(frame) = body.frame().await {
        let frame =
            frame.map_err(|_| failure(StatusCode::BAD_REQUEST, "The upload was interrupted."))?;
        if let Ok(data) = frame.into_data() {
            length += data.len();
            if length > size {
                return Err(failure(
                    StatusCode::BAD_REQUEST,
                    "The file changed during upload.",
                ));
            }
            if let Some(digest) = &mut digest {
                digest.update(&data);
            }
            output
                .write_all(&data)
                .await
                .map_err(|e| internal(e.into()))?;
        }
    }
    if length != size {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "The file changed during upload.",
        ));
    }
    if let (Some(digest), Some(checksum)) = (digest, checksum) {
        let actual = digest
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if actual != checksum {
            return Err(failure(
                StatusCode::BAD_REQUEST,
                "The file changed during upload.",
            ));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct DeviceUpload {
    path: String,
    modified: i64,
    digest: String,
    expected: Option<Uuid>,
    #[serde(default)]
    offer_match: bool,
}

async fn upload_device_file(
    State(app): State<Shared>,
    Path((project, folder)): Path<(Uuid, Uuid)>,
    Query(input): Query<DeviceUpload>,
    headers: HeaderMap,
    request: Request,
) -> Api<Json<serde_json::Value>> {
    let binding = device_binding(&headers)?;
    let size = upload_length(&headers)?;
    let stamp = library::DeviceStamp {
        size: size as u64,
        modified: input.modified,
        digest: input.digest,
    };
    stamp.validate(MAX_IMPORT as u64).map_err(bad)?;
    let _permit = app.import_slot.acquire().await.map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The import service is unavailable.",
        )
    })?;
    let (place, old, unlocked_format) = app
        .store
        .read(|db| -> Result<_> {
            let project = db
                .projects
                .iter()
                .find(|item| item.id == project)
                .context("Project not found.")?;
            let (place, id) = project
                .library
                .device_destination(folder, binding, &input.path)?;
            let source = id.and_then(|id| project.source(id)).cloned();
            ensure!(
                source.as_ref().map(Source::storage_id) == input.expected,
                "The device file changed in the studio. Scan again."
            );
            Ok((place, source, project.sequence.offers_source_format()))
        })
        .map_err(bad)?;
    let dir = media::temp(&app.store).await.map_err(internal)?;
    let result = async {
        let file = dir.join("device.media");
        receive_upload(request, &file, size, Some(&stamp.digest)).await?;
        let mut source = media::import(&app.store, &file, place.title())
            .await
            .map_err(bad)?;
        source.device_digest = Some(stamp.digest.clone());
        let offer = if input.offer_match && old.is_none() && unlocked_format {
            media::sequence_offer(&app.store, &source).await
        } else {
            None
        };
        let committed = app.store.update(|db| {
            let project = in_project(db, project)?;
            let (place, id) = project
                .library
                .device_destination(folder, binding, &input.path)?;
            let current = id.and_then(|id| project.source(id));
            ensure!(
                current.map(Source::storage_id) == input.expected,
                "The device file changed in the studio. Scan again."
            );
            if let Some(prior) = &old {
                ensure!(
                    prior.same_kind(&source),
                    "Choose the same media kind for this source."
                );
            }
            let id = id.unwrap_or(source.id);
            project.library.set_device_file(place, id, stamp);
            if old.is_some() {
                project.replace_sources(vec![(id, source.clone())]);
            } else {
                project.sources.push(source.clone());
                project.library_revision += 1;
                project.offer_sequence_format(offer);
            }
            Ok(project.library.source_json(project.source(id).unwrap()))
        });
        match committed {
            Ok(value) => Ok(Json(value)),
            Err(error) => {
                media::discard_import(&app.store, &source).await;
                Err(bad(error))
            }
        }
    }
    .await;
    let _ = tokio::fs::remove_dir_all(dir).await;
    result
}

#[derive(Deserialize)]
struct SourceVersion {
    v: Option<Uuid>,
    draft: Option<Uuid>,
    frame: Option<u64>,
}

fn preview_frame(app: &App, query: &SourceVersion) -> Option<Arc<crate::director_preview::Frame>> {
    let id = query.draft?;
    if let Some(frame) = app.mcp_activity.frame(id, query.frame) {
        return Some(frame);
    }
    let allowed = app.store.read(|db| {
        db.runs
            .iter()
            .any(|run| run.id == id && run.pending() && !run.stop_requested)
    });
    if !allowed {
        return None;
    }
    feed(app, id)?.frame(query.frame)
}

fn preview_or_saved_project(app: &App, id: Uuid, query: &SourceVersion) -> Option<Project> {
    preview_frame(app, query)
        .filter(|frame| frame.project.id == id)
        .map(|frame| frame.project.clone())
        .or_else(|| {
            app.store
                .read(|db| db.projects.iter().find(|project| project.id == id).cloned())
        })
}

fn published_source(app: &App, id: Uuid, version: &SourceVersion) -> Option<Source> {
    let matches =
        |source: &Source| source.id == id && version.v.is_none_or(|id| id == source.storage_id());
    app.store
        .read(|db| {
            db.projects
                .iter()
                .filter(|project| !project.library.missing.contains_key(&id))
                .find_map(|project| project.sources.iter().find(|source| matches(source)))
                .cloned()
        })
        .or_else(|| {
            let frame = preview_frame(app, version)?;
            if frame.project.library.missing.contains_key(&id) {
                return None;
            }
            frame
                .project
                .sources
                .iter()
                .find(|source| matches(source))
                .cloned()
        })
}

async fn media_file(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Query(version): Query<SourceVersion>,
    request: Request,
) -> Response {
    let Some(source) = published_source(&app, id, &version) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let path = match crate::source_color::playback(&app.store, &source).await {
        Ok(path) => path,
        Err(error) => {
            return failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Could not prepare the source for playback. {error}"),
            )
            .into_response();
        }
    };
    ServeFile::new(path)
        .oneshot(request)
        .await
        .expect("file service")
        .into_response()
}

#[derive(Deserialize)]
struct MediaDownload {
    v: Option<Uuid>,
}

async fn download_source(
    State(app): State<Shared>,
    Path((project, asset)): Path<(Uuid, Uuid)>,
    Query(download): Query<MediaDownload>,
    request: Request,
) -> Api<Response> {
    let source = app
        .store
        .read(|db| {
            let project = db.projects.iter().find(|item| item.id == project)?;
            let source = project.source(asset)?;
            (!project.library.missing.contains_key(&asset)
                && download
                    .v
                    .is_none_or(|version| source.storage_id() == version))
            .then(|| source.clone())
        })
        .ok_or_else(|| {
            failure(
                StatusCode::NOT_FOUND,
                "Source is unavailable in this project.",
            )
        })?;
    let _permit = app.import_slot.acquire().await.map_err(|_| {
        failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Media service is unavailable.",
        )
    })?;
    let dir = media::temp(&app.store).await.map_err(internal)?;
    let response = async {
        let (file, extension, _) = room::portable_media(&app.store, &source, &dir)
            .await
            .map_err(|_| {
                failure(
                    StatusCode::NOT_FOUND,
                    "The saved source file is unavailable.",
                )
            })?;
        let mut response = ServeFile::new(file)
            .oneshot(request)
            .await
            .expect("file service")
            .into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        set_download_filename(
            &mut response,
            &room::source_filename(&source.title, &extension),
        );
        Ok(response)
    }
    .await;
    let _ = tokio::fs::remove_dir_all(dir).await;
    response
}

async fn timed_audio_file(
    State(app): State<Shared>,
    Path((project, clip, key)): Path<(Uuid, Uuid, String)>,
    Query(version): Query<SourceVersion>,
    request: Request,
) -> Api<Response> {
    let current = |project: &Project| {
        let clip = project.timeline.iter().find(|item| item.id == clip)?;
        let source = project.source(clip.source).filter(|item| item.audio)?;
        (media::timed_audio_key(clip, source).as_deref() == Some(key.as_str()))
            .then(|| (source.clone(), clip.clone()))
    };
    let pair = app
        .store
        .read(|db| current(db.projects.iter().find(|item| item.id == project)?))
        .or_else(|| {
            let view = preview_frame(&app, &version)?;
            if view.project.id != project {
                return None;
            }
            current(&view.project)
        });
    let (source, clip) = pair.ok_or_else(|| {
        failure(
            StatusCode::NOT_FOUND,
            "That audio preview is no longer current.",
        )
    })?;
    let path = media::timed_audio(&app.store, &source, &clip)
        .await
        .map_err(bad)?
        .ok_or_else(|| {
            failure(
                StatusCode::NOT_FOUND,
                "That audio preview is no longer current.",
            )
        })?;
    Ok(ServeFile::new(path)
        .oneshot(request)
        .await
        .expect("file service")
        .into_response())
}

async fn export(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    input: Option<Json<media::ExportOptions>>,
) -> Api<Response> {
    let options = input.map(|Json(options)| options).unwrap_or_default();
    let wav = options.format.as_deref() == Some("audio");
    let _permit = app.export_slot.try_acquire().map_err(|_| {
        failure(
            StatusCode::TOO_MANY_REQUESTS,
            "Wait for the other export to finish.",
        )
    })?;
    let project = app
        .store
        .read(|db| db.projects.iter().find(|p| p.id == id).cloned())
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Project not found."))?;
    let dir = media::temp(&app.store).await.map_err(internal)?;
    let result = async {
        let path = media::export(&app.store, &project, &dir, &options)
            .await
            .map_err(bad)?;
        let size = tokio::fs::metadata(&path)
            .await
            .map_err(|e| internal(e.into()))?
            .len();
        if size > 256 * 1024 * 1024 {
            return Err(failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Export a shorter cut.",
            ));
        }
        let data = tokio::fs::read(&path)
            .await
            .map_err(|e| internal(e.into()))?;
        Ok((
            [
                (
                    header::CONTENT_TYPE,
                    if wav { "audio/wav" } else { "video/mp4" },
                ),
                (
                    header::CONTENT_DISPOSITION,
                    if wav {
                        "attachment; filename=\"editroom.wav\""
                    } else {
                        "attachment; filename=\"editroom.mp4\""
                    },
                ),
            ],
            Body::from(data),
        )
            .into_response())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(dir).await;
    result
}

#[derive(Deserialize, Serialize)]
struct Grant {
    amount: u32,
}

async fn grant(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(input): Json<Grant>,
) -> Api<StatusCode> {
    if app.store.account || secure_host(&app, &headers) {
        return Err(failure(
            StatusCode::FORBIDDEN,
            "Add local test credits from the computer running EditCastle.",
        ));
    }
    if !(1..=MAX_LOCAL_TEST_CREDITS).contains(&input.amount) {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Add between 1 and 5,000 local test credits.",
        ));
    }
    app.store
        .update(|db| {
            db.granted = db.granted.saturating_add(input.amount);
            Ok(())
        })
        .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

fn thumb_path(store: &Store, id: Uuid) -> PathBuf {
    store.media(id, true).with_extension("thumb-v2.jpg")
}

async fn thumbnail(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Query(version): Query<SourceVersion>,
    request: Request,
) -> Response {
    let source = published_source(&app, id, &version).filter(|source| source.video);
    let Some(source) = source else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let input = match crate::source_color::playback(&app.store, &source).await {
        Ok(input) => input,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let path = input.with_extension("thumb-v2.jpg");
    if !path.exists() {
        let work = match media::temp(&app.store).await {
            Ok(work) => work,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        let temporary = work.join("thumb.jpg");
        if media::extract(&input, 0, &temporary, true).await.is_ok() {
            let _ = tokio::fs::rename(&temporary, &path).await;
        }
        let _ = tokio::fs::remove_dir_all(work).await;
    }
    ServeFile::new(path)
        .oneshot(request)
        .await
        .expect("file service")
        .into_response()
}

async fn waveform_file(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Query(version): Query<SourceVersion>,
    request: Request,
) -> Response {
    let source = published_source(&app, id, &version).filter(|source| source.audio);
    let Some(source) = source else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let media_file = app.store.media(source.storage_id(), source.video);
    let destination = media_file.with_extension("wave.png");
    if !destination.exists() {
        let work = match media::temp(&app.store).await {
            Ok(work) => work,
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        };
        let staged = work.join("wave.png");
        if media::waveform(&media_file, &staged).await.is_ok() {
            let _ = tokio::fs::rename(&staged, &destination).await;
        }
        let _ = tokio::fs::remove_dir_all(work).await;
    }
    ServeFile::new(destination)
        .oneshot(request)
        .await
        .expect("file service")
        .into_response()
}

async fn waveform_levels(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    Query(version): Query<SourceVersion>,
) -> Api<impl IntoResponse> {
    let source = published_source(&app, id, &version)
        .filter(|source| source.audio)
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "Audio source not found."))?;
    let levels = crate::audio::waveform_levels(&app.store, &source)
        .await
        .map_err(internal)?;
    Ok((
        [
            ("content-type", "application/octet-stream".to_owned()),
            (
                "x-editroom-waveform-rate",
                crate::audio::WAVEFORM_RATE.to_string(),
            ),
        ],
        levels,
    ))
}

async fn mcp_rpc(
    State(app): State<Shared>,
    Path(project_id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<serde_json::Value>,
) -> Response {
    let id = &input["id"];
    let result = |value| Json(json!({"jsonrpc":"2.0","id":id,"result":value})).into_response();
    let error = |code, message| {
        Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}))
            .into_response()
    };
    let method = input["method"].as_str().unwrap_or_default();
    let Some(owner) = mcp_owner(&app, &headers) else {
        return failure(
            StatusCode::UNAUTHORIZED,
            "Sign in or configure an attached-agent key.",
        )
        .into_response();
    };
    let may_generate = matches!(&owner, mcp_activity::Owner::Browser(_));
    if input["jsonrpc"] != "2.0" {
        return error(-32600, "Invalid Request");
    }
    let agent = if method != "initialize"
        && let Some(session) = header_str(&headers, "mcp-session-id")
    {
        let actor = session
            .parse()
            .ok()
            .and_then(|id| app.mcp_activity.client(project_id, &owner, id));
        let Some(actor) = actor else {
            let mut response = failure(
                StatusCode::NOT_FOUND,
                "Initialize a new MCP session for this project.",
            )
            .into_response();
            // This request has not reached a tool. A client may safely resend it with a new session.
            response.headers_mut().insert(
                mcp_activity::SESSION_STATUS,
                HeaderValue::from_static(mcp_activity::EXPIRED),
            );
            return response;
        };
        Some(actor)
    } else if !may_generate {
        Some(mcp_activity::Agent::anonymous())
    } else {
        None
    };
    if id.is_null() && method.starts_with("notifications/") {
        return StatusCode::ACCEPTED.into_response();
    }
    match method {
        "initialize" => {
            let asked = input["params"]["protocolVersion"]
                .as_str()
                .unwrap_or("2025-06-18");
            let version = if ["2025-06-18", "2025-03-26", "2024-11-05"].contains(&asked) {
                asked
            } else {
                "2025-06-18"
            };
            if !app
                .store
                .read(|db| db.projects.iter().any(|project| project.id == project_id))
            {
                return failure(StatusCode::NOT_FOUND, "Project not found.").into_response();
            }
            let mut response = result(
                json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},
                "serverInfo":{"name":"editroom","title":"EditCastle","version":"1.0"},
                "instructions":"Call get_project to read the current cut and list_media for source IDs. Timeline times are in seconds. Edits are applied to the project identified by this MCP URL. The browser shows edits as they happen."}),
            );
            {
                let Ok(_access) = confirm_attached_access(&app, &owner) else {
                    return failure(
                        StatusCode::UNAUTHORIZED,
                        "Attached-agent access has changed.",
                    )
                    .into_response();
                };
                let name = input["params"]["clientInfo"]["name"]
                    .as_str()
                    .unwrap_or("MCP agent");
                let session = app.mcp_activity.add_client(project_id, owner, name);
                response.headers_mut().insert(
                    "mcp-session-id",
                    HeaderValue::from_str(&session.to_string()).expect("session header"),
                );
            }
            response
        }
        "ping" => result(json!({})),
        "tools/list" => result(json!({"tools":exposed_tools(may_generate)})),
        "tools/call" => {
            let method = input["params"]["name"].as_str().unwrap_or_default();
            let args = &input["params"]["arguments"];
            if !exposed_tools(may_generate)
                .iter()
                .any(|tool| tool["name"] == method)
            {
                return error(-32602, "Unknown or unavailable tool");
            }
            let activity = agent
                .as_ref()
                .map(|agent| (app.mcp_activity.channel(project_id), agent));
            if let Some((channel, agent)) = &activity {
                let asset = matches!(method, "look_at_media" | "analyze_media" | "get_transcript")
                    .then(|| {
                        args["asset_id"]
                            .as_str()
                            .and_then(|id| id.parse::<Uuid>().ok())
                    })
                    .flatten();
                channel.focus(agent, tools::label(method), asset);
            }
            if method == "save_project" {
                let Ok(_access) = confirm_attached_access(&app, &owner) else {
                    return failure(
                        StatusCode::UNAUTHORIZED,
                        "Attached-agent access has changed.",
                    )
                    .into_response();
                };
                return match app.store.save_project(project_id) {
                    Ok(()) => result(json!({
                        "content":[{"type":"text","text":json!({"saved":true,"project":project_id}).to_string()}],
                        "isError":false
                    })),
                    Err(error) => result(json!({
                        "content":[{"type":"text","text":error.to_string()}],
                        "isError":true
                    })),
                };
            }
            if let Some(special) = special_tool(
                &app,
                project_id,
                method,
                args,
                if method == "studio_render_video" {
                    with_session(&app, &headers, Session::provider)
                        .ok()
                        .flatten()
                } else {
                    None
                },
            )
            .await
            {
                if confirm_attached_access(&app, &owner).is_err() {
                    return failure(
                        StatusCode::UNAUTHORIZED,
                        "Attached-agent access has changed.",
                    )
                    .into_response();
                }
                let data = match special {
                    Ok(value) => {
                        json!({"content":[{"type":"text","text":value.to_string()}],"isError":false})
                    }
                    Err(error) => {
                        json!({"content":[{"type":"text","text":error.to_string()}],"isError":true})
                    }
                };
                return result(data);
            }
            let _turn = if let Some((channel, _)) = &activity
                && mcp_changes_project(method, args)
            {
                match mcp_edit_turn(&app, project_id, channel).await {
                    Ok(turn) => Some(turn),
                    Err(error) => {
                        return result(
                            json!({"content":[{"type":"text","text":error.to_string()}],"isError":true}),
                        );
                    }
                }
            } else {
                None
            };
            let project = app
                .store
                .read(|db| db.projects.iter().find(|p| p.id == project_id).cloned());
            let Some(mut project) = project else {
                return error(-32602, "Project not found");
            };
            let transport = app.playback.view(&project);
            project.sequence.playhead = transport.at;
            project.sequence.playing = transport.playing;
            let work = match media::temp(&app.store).await {
                Ok(work) => work,
                Err(_) => return error(-32603, "Workspace unavailable"),
            };
            let before_library = project.library_revision;
            let original = activity
                .as_ref()
                .filter(|(channel, _)| channel.has_viewer() && mcp_changes_project(method, args))
                .map(|_| project.clone());
            let called = tools::call(method, args, &mut project, &app.store, &work).await;
            let _ = tokio::fs::remove_dir_all(work).await;
            let called = match called {
                Ok(reply) if reply.changed => {
                    async {
                        let preview = if let Some(base) = original
                            && let Some((channel, agent)) = &activity
                            && channel.has_viewer()
                        {
                            let edits = visible_edits(&base, &project, method, 0)
                                .into_iter()
                                .take(80)
                                .collect::<Vec<_>>();
                            if edits.is_empty() {
                                None
                            } else {
                                Some(channel.prepare(
                                    agent,
                                    tools::label(method),
                                    crate::director_preview::Frame::new(1, &project, edits),
                                )?)
                            }
                        } else {
                            None
                        };
                        if preview.is_some() {
                            tokio::time::sleep(mcp_activity::ANIMATION).await;
                        }
                        let _access = confirm_attached_access(&app, &owner)?;
                        let revision = app.store.update(|db| {
                            let current = in_project(db, project_id)?;
                            ensure!(
                                current.revision == project.revision,
                                "The project changed while that tool was running."
                            );
                            if project.library_revision != before_library {
                                ensure!(
                                    current.library_revision == before_library,
                                    "The media library changed during that edit."
                                );
                                current.library = project.library.clone();
                                current.sources = project.sources.clone();
                                current.library_revision = project.library_revision;
                            }
                            sync_edit(current, project);
                            Ok(current.revision)
                        })?;
                        if let Some(preview) = preview {
                            preview.finish(revision);
                        }
                        Ok(reply)
                    }
                    .await
                }
                other => other,
            };
            if confirm_attached_access(&app, &owner).is_err() {
                return failure(
                    StatusCode::UNAUTHORIZED,
                    "Attached-agent access has changed.",
                )
                .into_response();
            }
            let (mut content, is_error) = match called {
                Ok(reply) => {
                    let mut content = vec![json!({"type":"text","text":reply.value.to_string()})];
                    content.extend(
                        reply.images.into_iter().map(
                            |data| json!({"type":"image","data":data,"mimeType":"image/jpeg"}),
                        ),
                    );
                    (content, false)
                }
                Err(error) => (vec![json!({"type":"text","text":error.to_string()})], true),
            };
            result(json!({"content":std::mem::take(&mut content),"isError":is_error}))
        }
        _ => error(-32601, "Method not found"),
    }
}

fn mcp_owner(app: &App, headers: &HeaderMap) -> Option<mcp_activity::Owner> {
    if headers.contains_key(header::AUTHORIZATION) {
        return attached_owner(app, headers);
    }
    with_session(app, headers, |_| ()).ok()?;
    session_id(headers).map(|session| mcp_activity::Owner::Browser(session.to_owned()))
}

fn confirm_attached_access<'a>(
    app: &'a App,
    owner: &mcp_activity::Owner,
) -> Result<Option<std::sync::MutexGuard<'a, crate::attached_agents::Access>>> {
    let mcp_activity::Owner::Bearer(revision) = owner else {
        return Ok(None);
    };
    let access = app.attached_agents.lock().expect("attached agent access");
    ensure!(
        access.accepts(*revision),
        "Attached-agent access has changed."
    );
    Ok(Some(access))
}

async fn mcp_disconnect(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    headers: HeaderMap,
) -> StatusCode {
    let owner = mcp_owner(&app, &headers);
    let id = header_str(&headers, "mcp-session-id").and_then(|id| id.parse().ok());
    if id
        .zip(owner)
        .is_some_and(|(id, owner)| app.mcp_activity.close_client(project, &owner, id))
    {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::NOT_FOUND
    }
}

#[derive(Deserialize)]
struct McpActivityCursor {
    viewer: Uuid,
    after: Option<u64>,
}

async fn mcp_activity_feed(
    State(app): State<Shared>,
    Path(project): Path<Uuid>,
    Query(cursor): Query<McpActivityCursor>,
    headers: HeaderMap,
) -> Api<Json<serde_json::Value>> {
    if !app
        .store
        .read(|db| db.projects.iter().any(|entry| entry.id == project))
    {
        return Err(failure(StatusCode::NOT_FOUND, "Project not found."));
    }
    let packet = app
        .mcp_activity
        .channel(project)
        .observe(cursor.viewer, cursor.after)
        .await;
    with_session(&app, &headers, |_| ())?;
    Ok(Json(packet))
}

fn mcp_changes_project(tool: &str, args: &serde_json::Value) -> bool {
    !matches!(
        tool,
        "get_project"
            | "get_frame"
            | "get_captions"
            | "get_transcript"
            | "list_media"
            | "list_looks"
            | "look_at_media"
            | "analyze_media"
            | "search_media"
            | "copy_clips"
            | "save_project"
    ) && !(tool == "remove_media" && args["preview"] == true)
}

async fn mcp_edit_turn(
    app: &App,
    project: Uuid,
    channel: &mcp_activity::Channel,
) -> Result<tokio::sync::OwnedMutexGuard<()>> {
    tokio::time::timeout(Duration::from_secs(100), async {
        let guard = channel.edit.clone().lock_owned().await;
        loop {
            let running = app.store.read(|db| {
                db.runs
                    .iter()
                    .find(|run| run.project == project && run.pending())
                    .map(|run| run.id)
            });
            let Some(id) = running else {
                return guard;
            };
            if let Some(active) = feed(app, id) {
                active.closed().await;
            }
            tokio::time::sleep(Duration::from_millis(65)).await;
        }
    })
    .await
    .context("The current Director edit is still active. Retry this tool when it finishes.")
}

fn sync_edit(current: &mut Project, mut changed: Project) {
    for source in std::mem::take(&mut changed.sources) {
        if let Some(previous) = current.sources.iter_mut().find(|item| item.id == source.id) {
            previous.notes = source.notes;
        } else {
            current.sources.push(source);
        }
    }
    sync_cut(current, changed);
}

fn sync_cut(current: &mut Project, changed: Project) {
    current.timeline = changed.timeline;
    current.soundtrack = changed.soundtrack;
    let playhead = current.sequence.playhead;
    current.sequence = changed.sequence;
    current.sequence.playhead = playhead;
    current.sequence.playing = false;
    current.sequence_offer = changed.sequence_offer;
    current.undo_steps = changed.undo_steps;
    current.redo_steps = changed.redo_steps;
    current.revision += 1;
}

async fn agent_direct(
    app: &Shared,
    provider: &Provider,
    project: &Project,
    input: Ask,
    run: Uuid,
) -> Result<DirectorReply> {
    event(app, run, "Opened the edit draft");
    let work = media::temp(&app.store).await?;
    let mut generated = Vec::new();
    let mut active = None;
    let result = agent_draft(
        app,
        provider,
        project,
        input,
        run,
        &work,
        &mut generated,
        &mut active,
    )
    .await;
    if let Some(active) = active {
        if !active.task.is_finished()
            && let Some(preview) = feed(app, run)
        {
            preview.close();
        }
        let _ = active.task.await;
    }
    let _ = tokio::fs::remove_dir_all(work).await;
    result
}

async fn install_reviewed_shot(
    store: &Store,
    run: Uuid,
    source: &Source,
    work: &std::path::Path,
) -> Result<PathBuf> {
    store.record_reviewed_shot(run, source, work)?;
    let target = store.media(source.id, true);
    let staged = work.join(format!("{}-visible.mp4", source.id));
    tokio::fs::rename(staged, &target).await?;
    Ok(target)
}

struct Candidate {
    id: Uuid,
    title: String,
    audible: bool,
    visible: u32,
    video: PathBuf,
    anchor: Option<PathBuf>,
    review_round: Option<usize>,
    decided: bool,
}

struct CandidateRender {
    id: Uuid,
    task: tokio::task::JoinHandle<Result<Candidate>>,
}

struct CandidateRequest {
    title: String,
    direction: String,
    visible: u32,
    rendered: u32,
    credits: u32,
    reference: Option<(Project, Clip)>,
}

async fn render_candidate(
    app: &App,
    worker: &Worker,
    preview: &Feed,
    run: Uuid,
    id: Uuid,
    request: &CandidateRequest,
    work: &std::path::Path,
) -> Result<Candidate> {
    let title = &request.title;
    let prepare = Box::pin(async {
        worker.check().await?;
        let mut direction = request.direction.clone();
        let (worker_image, anchor) = if let Some((project, clip)) = &request.reference {
            let (image, thumb) = media::continuation_frame(&app.store, project, clip, work).await?;
            direction.push_str("\nBegin exactly from the supplied frame. Continue the action immediately in the same physical direction. Preserve the established subject and scene.");
            event(
                app,
                run,
                format!("Captured the last visible frame · {}", clip.name(project)),
            );
            let file = worker.upload(id, &image).await.map_err(|_| {
                anyhow::anyhow!("Could not transfer the last visible frame to ComfyUI.")
            })?;
            (Some(file), Some(thumb))
        } else {
            (None, None)
        };
        Ok::<_, anyhow::Error>((direction, worker_image, anchor))
    });
    let (direction, worker_image, anchor) = tokio::select! {
        result = prepare => result?,
        _ = preview.closed() => bail!("Director stopped. The draft was discarded."),
    };
    ensure!(!preview.is_closed(), "Director closed the draft.");
    check_stop(app, run)?;
    let (filename, folder) = run_worker_shot(
        app,
        worker,
        run,
        WorkerShot {
            id,
            title,
            text: &direction,
            visible: request.visible,
            rendered: request.rendered,
            credits: request.credits,
            anchor: worker_image,
            reference: anchor.as_deref(),
        },
    )
    .await?;
    check_stop(app, run)?;
    let raw = work.join(format!("{id}-raw.mp4"));
    let video = work.join(format!("{id}-visible.mp4"));
    let download = Box::pin(async {
        worker
            .download(filename, folder, &raw, |stage| {
                event(app, run, format!("{stage} · {title}"));
            })
            .await?;
        media::cut_render(&raw, &video, request.visible).await
    });
    let meta = tokio::select! {
        result = download => result?,
        _ = preview.closed() => bail!("Director stopped. The draft was discarded."),
    };
    let _ = tokio::fs::remove_file(raw).await;
    check_stop(app, run)?;
    event(
        app,
        run,
        format!(
            "Rendered {title} · {:.2}s · {}",
            f64::from(request.visible) / 24.0,
            worker.label(),
        ),
    );
    Ok(Candidate {
        id,
        title: title.clone(),
        audible: meta.audio,
        visible: request.visible,
        video,
        anchor,
        review_round: None,
        decided: false,
    })
}

struct EditDraft<'a> {
    app: &'a Shared,
    project: Project,
    library_base: editor::LibrarySnapshot,
    run: Uuid,
    work: &'a std::path::Path,
    candidates: HashMap<Uuid, std::result::Result<Candidate, String>>,
    active: &'a mut Option<CandidateRender>,
    generated: &'a mut Vec<Source>,
    changed: bool,
    review_count: usize,
    preferred_duration: Option<f64>,
    edits: Vec<VisibleEdit>,
    citations: Vec<Citation>,
}

async fn agent_draft(
    app: &Shared,
    provider: &Provider,
    project: &Project,
    input: Ask,
    run: Uuid,
    work: &std::path::Path,
    generated: &mut Vec<Source>,
    active: &mut Option<CandidateRender>,
) -> Result<DirectorReply> {
    let at = input
        .playhead
        .unwrap_or_else(|| app.playback.view(project).at);
    let mut selected = input.selected_clips.clone();
    if let Some(primary) = input.selected
        && !selected.contains(&primary)
    {
        selected.push(primary);
    }
    let edit_context = director_prompt::context(project, &input.prompt, at, &selected);
    let body = json!({
        "request":input.prompt,"selected_clip":input.selected,"selected_clips":input.selected_clips,
        "selected_media":input.selected_media,"seconds_per_shot":input.duration,
        "playhead_seconds":clock::seconds(at),"edit_context":edit_context,
        "recent_conversation":crate::studio_history::context(&project.messages, 1, 8),
    });
    let mut agent = Agent::new(provider.clone(), body).await?;
    let mut draft = EditDraft {
        app,
        project: project.clone(),
        library_base: project.capture_library(),
        run,
        work,
        candidates: HashMap::new(),
        active,
        generated,
        changed: false,
        review_count: 0,
        preferred_duration: input.duration,
        edits: vec![],
        citations: vec![],
    };
    draft.project.undo_steps.clear();
    draft.project.redo_steps.clear();
    draft.project.sequence.playhead = at;
    draft.project.sequence.playing = false;
    let preview = feed(app, run).context("Director preview is unavailable.")?;
    for round in 0..120 {
        check_stop(app, run)?;
        let response = tokio::select! {
            result=agent.turn(&app.client, |message| event(app, run, message))=>result,
            _=preview.closed()=>bail!("Director stopped. The draft was discarded."),
        };
        check_stop(app, run)?;
        let mut turn = match response {
            Ok(turn) => turn,
            Err(error) => return draft.finish_incomplete(project, error),
        };
        for activity in &turn.activity {
            event(app, run, *activity);
        }
        for message in std::mem::take(&mut turn.preambles) {
            event(app, run, message);
        }
        if turn.continue_turn {
            draft.remember_citations(turn.citations);
            continue;
        }
        if turn.calls.is_empty() {
            let lead = turn.answer.chars().count() - turn.answer.trim_start().chars().count();
            let final_text: String = turn.answer.trim().chars().take(3000).collect();
            if final_text.is_empty() {
                draft.remember_citations(turn.citations);
                return draft.finish_incomplete(
                    project,
                    anyhow::anyhow!("Director finished without an answer."),
                );
            }
            let length = final_text.chars().count();
            for citation in &mut turn.citations {
                citation.after = citation
                    .after
                    .map(|index| index.saturating_sub(lead).min(length));
            }
            for citation in draft.citations.drain(..) {
                if !turn
                    .citations
                    .iter()
                    .any(|source| source.url == citation.url)
                {
                    turn.citations.push(citation);
                }
            }
            draft.commit(project)?;
            return Ok(Message {
                from: "director".into(),
                text: final_text,
                citations: turn.citations,
                studio: None,
            }
            .into());
        }
        draft.remember_citations(turn.citations);
        let mut results = vec![];
        for call in turn.calls {
            check_stop(app, run)?;
            event(app, run, tools::label(&call.name));
            let snapshot = draft.project.clone();
            let reply = Box::pin(draft.execute(&call.name, &call.args, round)).await;
            if let Ok(reply) = &reply {
                draft.changed |= reply.changed;
                if reply.changed
                    || snapshot.library_revision != draft.project.library_revision
                    || snapshot.sources != draft.project.sources
                {
                    let edits =
                        visible_edits(&snapshot, &draft.project, &call.name, draft.edits.len())
                            .into_iter()
                            .take(40)
                            .collect::<Vec<_>>();
                    preview.push(&draft.project, edits.clone());
                    draft
                        .edits
                        .extend(edits.into_iter().take(40 - draft.edits.len()));
                }
            }
            results.push((call.id, reply));
        }
        agent.respond(results);
    }
    draft.finish_incomplete(
        project,
        anyhow::anyhow!("Director reached the limit of tool calls before finishing the draft."),
    )
}

impl EditDraft<'_> {
    fn finish_incomplete(mut self, base: &Project, error: anyhow::Error) -> Result<DirectorReply> {
        check_stop(self.app, self.run)?;
        if !self.changed {
            return Err(error);
        }
        let reason: String = error.to_string().chars().take(600).collect();
        let citations = std::mem::take(&mut self.citations);
        self.commit(base)?;
        Ok(DirectorReply {
            message: Message {
                from: "director".into(),
                text: format!("Director saved the edits it had completed.\n\n{reason}"),
                citations,
                studio: None,
            },
            partial: true,
        })
    }

    fn commit(mut self, base: &Project) -> Result<()> {
        if !self.changed {
            return Ok(());
        }
        let app = self.app;
        let run = self.run;
        event(app, run, "Applying the draft to the timeline");
        app.store.update(|db| {
            record_director_commit(db, run)?;
            let current = in_project(db, self.project.id)?;
            if current.revision != base.revision {
                crate::draft_merge::rebase(base, &mut self.project, current)?;
                self.edits = visible_edits(current, &self.project, "edit", 0)
                    .into_iter()
                    .take(40)
                    .collect();
            }
            let prior = self.apply_library(current)?;
            self.project.undo_steps = current.undo_steps.clone();
            self.project.record(prior);
            sync_cut(current, self.project);
            current.validate_timeline(&current.timeline, current.soundtrack)?;
            let asset_ids = self
                .generated
                .iter()
                .filter(|source| current.source(source.id).is_some())
                .map(|source| source.id)
                .collect();
            if let Some(run) = db.runs.iter_mut().find(|item| item.id == run) {
                run.studio_turn_mut().changes = self.edits.clone();
                run.edits = self.edits;
                run.studio_turn_mut().asset_ids = asset_ids;
            }
            Ok(())
        })?;
        event(app, run, "Timeline updated");
        Ok(())
    }

    fn remember_citations(&mut self, citations: Vec<Citation>) {
        for mut citation in citations {
            if self.citations.len() >= 8 {
                break;
            }
            if self
                .citations
                .iter()
                .any(|source| source.url == citation.url)
            {
                continue;
            }
            citation.after = None;
            self.citations.push(citation);
        }
    }

    fn apply_library(&self, current: &mut Project) -> Result<editor::Snapshot> {
        let mut prior = current.snapshot();
        let desired = self.project.capture_library();
        if desired == self.library_base {
            return Ok(prior);
        }
        let mut before = current.capture_library();
        library::check_draft_changes(&self.library_base, &desired, &before)?;
        current.apply_library_delta(&self.library_base, &desired);
        let after = current.capture_library();
        // Reviewed renders remain in Media when the edit that placed them is undone.
        for generated in self.generated.iter() {
            if !before
                .sources
                .iter()
                .any(|source| source.id == generated.id)
                && let Some(source) = current.source(generated.id)
            {
                before.sources.push(source.clone());
            }
        }
        prior.library = Some(editor::LibraryChange { before, after });
        Ok(prior)
    }

    fn refresh_library(&mut self) -> Result<()> {
        let (live, revision) = self
            .app
            .store
            .read(|db| {
                db.projects
                    .iter()
                    .find(|p| p.id == self.project.id)
                    .map(|p| (p.capture_library(), p.library_revision))
            })
            .context("Project not found.")?;
        let desired = self.project.capture_library();
        library::check_draft_changes(&self.library_base, &desired, &live)?;
        self.project.sources = live.sources.clone();
        self.project.library = live.layout.clone();
        self.project.library_revision = revision.max(self.project.library_revision);
        self.project
            .apply_library_delta(&self.library_base, &desired);
        self.library_base = live;
        Ok(())
    }

    async fn execute(
        &mut self,
        name: &str,
        args: &serde_json::Value,
        round: usize,
    ) -> Result<tools::Reply> {
        match name {
            "render_video_shot" => Box::pin(self.render(args)).await,
            "review_video_shot" => Box::pin(self.review(args, round)).await,
            "decide_video_shot" => Box::pin(self.decide(args, round)).await,
            "list_media_folders" => Ok(tools::Reply::data(
                json!({"linked":linked_folders(&self.project),"inbox":library::scan(&self.app.store).await?.tree()}),
                false,
            )),
            "read_web_page" => {
                let address = args["url"].as_str().context("The page URL is needed.")?;
                let page = Box::pin(public_web::read_page(address)).await?;
                if let Some(citation) = Citation::new(
                    page["url"].as_str().unwrap_or_default(),
                    page["title"].as_str().unwrap_or_default(),
                    None,
                ) {
                    self.remember_citations(vec![citation]);
                }
                Ok(tools::Reply::data(page, false))
            }
            "import_media" | "download_media" | "sync_media_folders" => {
                let result = match name {
                    "import_media" => {
                        Box::pin(import_tool(self.app, self.project.id, args)).await?
                    }
                    "sync_media_folders" => {
                        Box::pin(inbox_action(
                            self.app,
                            self.project.id,
                            library::Action::Discover,
                            true,
                        ))
                        .await?
                    }
                    _ => {
                        let result =
                            Box::pin(download_tool(self.app, self.project.id, args)).await?;
                        let title = result["asset"]["title"]
                            .as_str()
                            .unwrap_or("Imported media");
                        let licence = result["licence"].as_str().unwrap_or_default();
                        if let Some(citation) = Citation::new(
                            result["source_page"].as_str().unwrap_or_default(),
                            &format!("{title} · {licence}"),
                            None,
                        ) {
                            self.citations.retain(|entry| entry.url != citation.url);
                            self.citations.insert(0, citation);
                            self.citations.truncate(8);
                        }
                        result
                    }
                };
                self.refresh_library()?;
                Ok(tools::Reply::data(result, false))
            }
            "save_project" => bail!("Save is available after the draft is applied."),
            _ => {
                Box::pin(tools::call(
                    name,
                    args,
                    &mut self.project,
                    &self.app.store,
                    self.work,
                ))
                .await
            }
        }
    }

    async fn render(&mut self, args: &serde_json::Value) -> Result<tools::Reply> {
        if let Some(active) = self.active.as_ref() {
            ensure!(
                active.task.is_finished(),
                "Wait for the current shot before rendering another."
            );
            self.finish_candidate(active.id).await;
        }
        check_stop(self.app, self.run)?;
        ensure!(
            self.candidates.len() < 48,
            "This draft has reached its render limit."
        );
        let field = |name: &str, max: usize| -> Result<&str> {
            let text = args[name].as_str().unwrap_or("").trim();
            ensure!(
                !text.is_empty() && text.chars().count() <= max,
                "{name} is needed."
            );
            Ok(text)
        };
        let title = field("title", 100)?.to_owned();
        let seconds = self
            .preferred_duration
            .or_else(|| args["duration_seconds"].as_f64())
            .context("duration_seconds is needed.")?;
        let (visible, rendered, credits) =
            timing(seconds).context("A shot must be between 1 and 15 seconds.")?;
        let id = Uuid::new_v4();
        let direction = format!(
            "Continuity: {}\nStarting pose: {}\nAction: {}\nEnding pose: {}",
            field("continuity", 2000)?,
            field("start_pose", 1000)?,
            field("action", 2000)?,
            field("end_pose", 1000)?
        );
        let reference = if let Some(reference) = args
            .get("continue_from_clip_id")
            .filter(|value| !value.is_null())
        {
            let clip_id = reference
                .as_str()
                .context("Use the prior placed video's clip ID.")?;
            let id: Uuid = clip_id
                .parse()
                .context("Use the prior placed video's clip ID.")?;
            let clip = self
                .project
                .timeline
                .iter()
                .find(|clip| clip.id == id && clip.kind == editor::ClipKind::Video)
                .context("Use a previously placed video clip as the starting frame.")?;
            Some((self.project.clone(), clip.clone()))
        } else {
            None
        };
        let app = self.app.clone();
        let run = self.run;
        let preview = feed(&app, run).context("Director preview is unavailable.")?;
        let work = self.work.to_path_buf();
        let worker = app.worker.model(self.project.sequence.render_model);
        let request = CandidateRequest {
            title,
            direction,
            visible,
            rendered,
            credits,
            reference,
        };
        let task = tokio::spawn(async move {
            event(
                &app,
                run,
                format!("Preparing {} · {} credits", request.title, request.credits),
            );
            let result = Box::pin(render_candidate(
                &app, &worker, &preview, run, id, &request, &work,
            ))
            .await;
            if result.is_err() && !preview.is_closed() {
                event(&app, run, format!("Could not render {}", request.title));
            }
            result
        });
        *self.active = Some(CandidateRender { id, task });
        Ok(tools::Reply::data(
            json!({"shot_id":id,"status":"rendering","duration_seconds":f64::from(visible)/24.0}),
            false,
        ))
    }

    async fn finish_candidate(&mut self, id: Uuid) -> bool {
        let Some(render) = self.active.as_mut().filter(|render| render.id == id) else {
            return true;
        };
        let Ok(result) = tokio::time::timeout(Duration::from_secs(75), &mut render.task).await
        else {
            return false;
        };
        self.active.take();
        self.candidates.insert(
            id,
            result
                .map_err(|_| "The renderer stopped unexpectedly.".to_owned())
                .and_then(|take| take.map_err(|error| error.to_string())),
        );
        true
    }

    async fn review(&mut self, args: &serde_json::Value, round: usize) -> Result<tools::Reply> {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let id: Uuid = args["shot_id"]
            .as_str()
            .context("Use the rendered shot ID.")?
            .parse()?;
        if !self.finish_candidate(id).await {
            return Ok(tools::Reply::data(
                json!({"shot_id":id,"status":"rendering",
                "message":"This shot is still rendering. Call review_video_shot again with the same shot_id."}),
                false,
            ));
        }
        check_stop(self.app, self.run)?;
        let shot = self
            .candidates
            .get_mut(&id)
            .context("No such candidate in this draft.")?
            .as_mut()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        ensure!(!shot.decided, "This take already has a decision.");
        ensure!(
            self.review_count < 60,
            "This draft has reached its footage review limit."
        );
        let mut output = tools::Reply::data(
            json!({"shot_id":id,"status":"ready","title":shot.title,"has_input_frame":shot.anchor.is_some()}),
            false,
        );
        let mut points = (0..8)
            .map(|part| ((shot.visible - 1) * part + 3) / 7)
            .collect::<Vec<_>>();
        if let Some(extra) = args["times"].as_array() {
            points.extend(
                extra
                    .iter()
                    .filter_map(serde_json::Value::as_f64)
                    .take(4)
                    .map(|time| ((time.max(0.0) * 24.0).round() as u32).min(shot.visible - 1)),
            );
        }
        points.sort_unstable();
        points.dedup();
        output.value["times"] = json!(
            points
                .iter()
                .map(|frame| f64::from(*frame) / 24.0)
                .collect::<Vec<_>>()
        );
        if let Some(image) = &shot.anchor {
            output
                .images
                .push(STANDARD.encode(tokio::fs::read(image).await?));
        }
        for point in points {
            let path = self.work.join(format!("{id}-{point}.jpg"));
            media::extract(&shot.video, point, &path, true).await?;
            output
                .images
                .push(STANDARD.encode(tokio::fs::read(path).await?));
        }
        check_stop(self.app, self.run)?;
        shot.review_round = Some(round);
        self.review_count += 1;
        event(
            self.app,
            self.run,
            format!("Checked frames from {}", shot.title),
        );
        Ok(output)
    }

    async fn decide(&mut self, args: &serde_json::Value, round: usize) -> Result<tools::Reply> {
        let id: Uuid = args["shot_id"]
            .as_str()
            .context("Use the rendered shot ID.")?
            .parse()?;
        let shot = self
            .candidates
            .get_mut(&id)
            .context("Review the shot's frames with review_video_shot before deciding.")?
            .as_mut()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        ensure!(!shot.decided, "This take already has a decision.");
        // The planner receives the images only after all calls in this reply finish.
        ensure!(
            shot.review_round.is_some_and(|review| review < round),
            "Wait for the review frames in the next reply before deciding on a take."
        );
        let accept = args["accept"]
            .as_bool()
            .context("Choose whether to accept the take.")?;
        let notes = args["notes"].as_str().unwrap_or("").trim();
        ensure!(
            !notes.is_empty() && notes.len() <= 1200,
            "Describe what happened in the footage."
        );
        if !accept {
            self.app.store.update(|db| {
                db.finish_render_receipt(id);
                Ok(())
            })?;
            shot.decided = true;
            event(
                self.app,
                self.run,
                format!("Rejected {} · {notes}", shot.title),
            );
            return Ok(tools::Reply::data(
                json!({"shot_id":id,"accepted":false}),
                false,
            ));
        }
        let source = Source {
            storage: None,
            origin_stamp: None,
            device_digest: None,
            id: shot.id,
            title: shot.title.clone(),
            duration: shot.visible * clock::MODEL_FRAME,
            video: true,
            audio: shot.audible,
            image: false,
            notes: notes.into(),
        };
        let target = install_reviewed_shot(&self.app.store, self.run, &source, self.work).await?;
        self.generated.push(source.clone());
        let _ = media::extract(&target, 0, &thumb_path(&self.app.store, id), true).await;
        self.project.sources.push(source);
        shot.decided = true;
        event(
            self.app,
            self.run,
            format!("Accepted {} · {notes}", shot.title),
        );
        Ok(tools::Reply::data(
            json!({"shot_id":id,"accepted":true,"asset_id":id,"duration_seconds":f64::from(shot.visible)/24.0}),
            true,
        ))
    }
}

/// The changes inside one clip that the browser performs as separate gestures.
fn edit_detail(old: Option<&Clip>, new: &Clip) -> Option<serde_json::Value> {
    let mut detail = serde_json::Map::new();
    let fx = &new.effects;
    let was = old.map(|clip| &clip.effects);
    let gain = was.map_or(0.0, |fx| fx.gain_db);
    if gain != fx.gain_db {
        detail.insert("gain".into(), json!([gain, fx.gain_db]));
    }
    let fade_in = was.map_or(0, |fx| fx.fade_in_frames);
    if fade_in != fx.fade_in_frames {
        detail.insert("fade_in".into(), json!([fade_in, fx.fade_in_frames]));
    }
    let fade_out = was.map_or(0, |fx| fx.fade_out_frames);
    if fade_out != fx.fade_out_frames {
        detail.insert("fade_out".into(), json!([fade_out, fx.fade_out_frames]));
    }
    let transition = was.and_then(|fx| fx.transition.clone());
    if transition != fx.transition {
        detail.insert("transition".into(), json!([transition, fx.transition]));
    }
    let grade = was.map_or(serde_json::Value::Null, |fx| fx.grade.clone());
    if grade != fx.grade {
        detail.insert("grade".into(), json!([grade, fx.grade]));
    }
    let title = was.map_or("", |fx| fx.title.as_str());
    if title != fx.title && !fx.title.is_empty() {
        detail.insert("typed".into(), json!(fx.title));
    }
    (!detail.is_empty()).then(|| serde_json::Value::Object(detail))
}

fn visible_edits(
    before: &Project,
    after: &Project,
    action: &str,
    batch: usize,
) -> Vec<VisibleEdit> {
    if matches!(action, "add_track" | "set_track" | "delete_track") {
        let (tracks, other) = if action == "delete_track" {
            (&before.sequence.tracks, &after.sequence.tracks)
        } else {
            (&after.sequence.tracks, &before.sequence.tracks)
        };
        return tracks
            .iter()
            .filter(|track| {
                let prior = other.iter().find(|item| item.id == track.id);
                match action {
                    "set_track" => prior.is_some_and(|prior| prior != *track),
                    _ => prior.is_none(),
                }
            })
            .map(|track| VisibleEdit {
                action: action.into(),
                batch: Some(batch),
                clip: track.id,
                before: (action != "add_track").then_some(0),
                after: (action != "delete_track").then_some(0),
                track: track.name.clone(),
                from_track: None,
                source: None,
                before_width: None,
                after_width: None,
                ruler_item: None,
                detail: None,
                track_id: Some(track.id),
                label: Some(match action {
                    "add_track" => format!("Added {}", track.name),
                    "delete_track" => format!("Deleted {}", track.name),
                    _ => format!("Adjusted {}", track.name),
                }),
            })
            .collect();
    }
    let mut changes = Vec::new();
    for clip in &after.timeline {
        let first = before.timeline.iter().find(|old| old.id == clip.id);
        let position = before.placement(clip.id);
        let updated = after.placement(clip.id);
        if !first.is_some_and(|old| {
            old.track() == clip.track()
                && old.start == clip.start
                && old.end == clip.end
                && old.source == clip.source
                && old.link == clip.link
                && old.mute == clip.mute
                && old.captioned == clip.captioned
                && old.effects.speed == clip.effects.speed
                && old.effects.ramp == clip.effects.ramp
                && old.effects.reverse == clip.effects.reverse
                && old.effects.freeze == clip.effects.freeze
                && old.effects.title == clip.effects.title
                && old.effects.color == clip.effects.color
                && old.duration() == clip.duration()
                && old.effects.gain_db == clip.effects.gain_db
                && old.effects.fade_in_frames == clip.effects.fade_in_frames
                && old.effects.fade_out_frames == clip.effects.fade_out_frames
                && old.effects.fade_in_curve == clip.effects.fade_in_curve
                && old.effects.fade_out_curve == clip.effects.fade_out_curve
                && old.effects.transition == clip.effects.transition
                && old.effects.transition_frames == clip.effects.transition_frames
                && old.effects.grade == clip.effects.grade
                && old.effects.transform == clip.effects.transform
                && position == updated
        }) {
            changes.push(VisibleEdit {
                action: action.into(),
                batch: Some(batch),
                clip: clip.id,
                before: position,
                after: updated,
                track: clip.track().into(),
                from_track: first.map(|clip| clip.track().to_owned()),
                source: (!clip.source.is_nil()).then_some(clip.source),
                ruler_item: None,
                track_id: None,
                label: Some(clip.name(after).to_owned()),
                before_width: first.map(Clip::duration),
                after_width: Some(clip.duration()),
                detail: edit_detail(first, clip),
            });
        }
    }
    for clip in &before.timeline {
        if after.timeline.iter().all(|new| new.id != clip.id) {
            changes.push(VisibleEdit {
                action: action.into(),
                batch: Some(batch),
                clip: clip.id,
                before: before.placement(clip.id),
                after: None,
                track: clip.track().into(),
                from_track: Some(clip.track().into()),
                source: (!clip.source.is_nil()).then_some(clip.source),
                ruler_item: None,
                track_id: None,
                label: Some(clip.name(before).to_owned()),
                before_width: Some(clip.duration()),
                after_width: None,
                detail: None,
            });
        }
    }
    let ruler = |id, before, after, kind: &str, label: String| VisibleEdit {
        action: action.into(),
        batch: Some(batch),
        clip: id,
        before,
        after,
        track: "Ruler".into(),
        from_track: Some("Ruler".into()),
        source: None,
        before_width: None,
        after_width: None,
        ruler_item: Some(kind.into()),
        track_id: None,
        label: Some(label),
        detail: None,
    };
    for marker in &after.sequence.markers {
        let old = before.sequence.markers.iter().find(|m| m.id == marker.id);
        if old != Some(marker) {
            changes.push(ruler(
                marker.id,
                old.map(|m| m.frame),
                Some(marker.frame),
                "marker",
                marker.name.clone(),
            ));
        }
    }
    for marker in &before.sequence.markers {
        if !after.sequence.markers.iter().any(|m| m.id == marker.id) {
            changes.push(ruler(
                marker.id,
                Some(marker.frame),
                None,
                "marker",
                marker.name.clone(),
            ));
        }
    }
    for (edge, old, new, caption) in [
        (
            "in",
            before.sequence.in_point,
            after.sequence.in_point,
            "In point",
        ),
        (
            "out",
            before.sequence.out_point,
            after.sequence.out_point,
            "Out point",
        ),
    ] {
        if old != new {
            changes.push(ruler(Uuid::nil(), old, new, edge, caption.into()));
        }
    }
    changes
}

const MCP_IO: &[&str] = &[
    "export",
    "export_status",
    "cancel_export",
    "import_media",
    "download_media",
    "list_media_folders",
    "sync_media_folders",
    "reload_media",
];
const MCP_RENDER: &[&str] = &["studio_render_video", "studio_video_status"];

fn exposed_tools(generation: bool) -> Vec<serde_json::Value> {
    let mut tools = tools::definitions(false);
    tools.extend(tools::native_definitions().into_iter().filter(|tool| {
        let name = tool["name"].as_str().unwrap_or_default();
        MCP_IO.contains(&name) || generation && MCP_RENDER.contains(&name)
    }));
    tools
}

async fn special_tool(
    app: &Shared,
    project: Uuid,
    name: &str,
    args: &serde_json::Value,
    provider: Option<Provider>,
) -> Option<Result<serde_json::Value>> {
    let operation = match name {
        "auto_edit" => match queue_auto_edit(app.clone(), project) {
            Ok((_, finished)) => finished
                .await
                .context("Auto Edit was interrupted.")
                .and_then(|result| result),
            Err(error) => Err(error),
        },
        "playback" => app.store.read(|db| {
            let project = db
                .projects
                .iter()
                .find(|item| item.id == project)
                .context("Project not found.")?;
            let command = playback::Command::from_tool(args, project)?;
            let view = app.playback.command(project, command)?;
            Ok(json!({"playhead":clock::seconds(view.at),"playing":view.playing}))
        }),
        "export" => start_export(app.clone(), project, args).await,
        "export_status" => Ok(json!(app.exports.list(Some(project)))),
        "cancel_export" => args["id"]
            .as_str()
            .and_then(|id| id.parse().ok())
            .context("Pass the export ID from export_status.")
            .and_then(|id| app.exports.cancel(id, Some(project)))
            .map(|job| json!(job)),
        "import_media" => import_tool(app, project, args).await,
        "list_media_folders" => inbox_folders(app, project).await,
        "sync_media_folders" => inbox_action(app, project, library::Action::Discover, true).await,
        "reload_media" => {
            async {
                let args = if args.is_null() {
                    json!({})
                } else {
                    args.clone()
                };
                let request: ReloadRequest =
                    serde_json::from_value(args).context("Choose a folder or inbox source IDs.")?;
                inbox_action(
                    app,
                    project,
                    library::Action::Reload(request.target()?),
                    true,
                )
                .await
            }
            .await
        }
        "download_media" => download_tool(app, project, args).await,
        "studio_render_video" => provider
            .context("Connect a planner for footage review.")
            .and_then(|provider| queue_quick_clips(app.clone(), project, args, provider)),
        "studio_video_status" => app.store.read(|db| {
            let id = args["run_id"]
                .as_str()
                .and_then(|id| id.parse::<Uuid>().ok());
            let summary = id.and_then(|id| {
                let current = db.projects.iter().find(|item| item.id == project)?;
                let run = db
                    .runs
                    .iter()
                    .find(|run| run.project == project && run.id == id);
                studio_history::run_status(current, run, id)
            });
            summary.context("No such run in this project.")
        }),
        _ => return None,
    };
    Some(operation)
}

async fn start_export(
    app: Shared,
    id: Uuid,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let project = app
        .store
        .read(|db| db.projects.iter().find(|project| project.id == id).cloned())
        .context("Project not found.")?;
    ensure!(!project.timeline.is_empty(), "The timeline is empty.");
    let options: media::ExportOptions =
        serde_json::from_value(args.clone()).context("Supply export options as an object.")?;
    let name = args["name"]
        .as_str()
        .or(project.export_name.as_deref())
        .unwrap_or(&project.title);
    let name = editor::export_file_stem(name).context("Enter a filename up to 100 characters.")?;
    let remember =
        args["remember_name"] == true && project.export_name.as_deref() != Some(name.as_str());
    let format = if options.format.as_deref() == Some("audio") {
        "wav"
    } else {
        "mp4"
    };
    let (job, updates) = app
        .exports
        .enqueue(project, options, format!("{name}.{format}"))?;
    let remembered = !remember
        || app
            .store
            .update(|db| {
                in_project(db, id)?.export_name = Some(name);
                Ok(())
            })
            .is_ok();
    let mut result = if args["wait"] == true {
        json!(export_queue::wait(updates).await)
    } else {
        json!(job)
    };
    if !remembered {
        result["preference_warning"] = json!("EditCastle could not remember the filename.");
    }
    Ok(result)
}

async fn export_list(State(app): State<Shared>) -> Json<Vec<export_queue::Job>> {
    Json(app.exports.list(None))
}

async fn clear_exports(State(app): State<Shared>) -> Api<StatusCode> {
    app.exports.clear_finished(None).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn cancel_export(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
) -> Api<Json<export_queue::Job>> {
    let job = app
        .exports
        .cancel(id, None)
        .map_err(|_| failure(StatusCode::NOT_FOUND, "No such export."))?;
    Ok(Json(job))
}

async fn export_download(
    State(app): State<Shared>,
    Path(id): Path<Uuid>,
    request: Request,
) -> Response {
    let job = app.exports.get(id);
    let Some(job) = job else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(path) = job.output else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = ServeFile::new(path)
        .oneshot(request)
        .await
        .expect("file service")
        .into_response();
    set_download_filename(&mut response, &job.name);
    response
}

fn set_download_filename(response: &mut Response, filename: &str) {
    let name: String = filename
        .chars()
        .map(|ch| {
            if (ch.is_ascii_graphic() || ch == ' ') && !['"', '\\', '/'].contains(&ch) {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let encoded = encoded_filename(filename);
    if let Ok(disposition) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{name}\"; filename*=UTF-8''{encoded}"
    )) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, disposition);
    }
}

fn encoded_filename(name: &str) -> String {
    name.bytes()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn linked_folders(project: &Project) -> Vec<serde_json::Value> {
    project
        .library
        .folders
        .iter()
        .map(library::Folder::public)
        .collect()
}

async fn inbox_folders(app: &App, project: Uuid) -> Result<serde_json::Value> {
    let linked = app
        .store
        .read(|db| {
            db.projects
                .iter()
                .find(|item| item.id == project)
                .map(linked_folders)
        })
        .context("Project not found.")?;
    let inbox = library::scan(&app.store).await?.tree();
    Ok(json!({"linked":linked,"inbox":inbox}))
}

async fn watch_inbox(app: Shared) {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let projects = app.store.read(|db| {
            db.projects
                .iter()
                .filter(|p| p.library.watches_inbox())
                .map(|p| p.id)
                .collect::<Vec<_>>()
        });
        if projects.is_empty() {
            continue;
        }
        let Ok(_permit) = app.import_slot.try_acquire() else {
            continue;
        };
        let Ok(inventory) = library::scan(&app.store).await else {
            continue;
        };
        let mut state = app.inbox_state.lock().await;
        for id in projects {
            let _ = library::apply(
                &app.store,
                id,
                &inventory,
                library::Action::Sync,
                MAX_IMPORT,
                &mut state,
            )
            .await;
        }
    }
}

async fn inbox_action(
    app: &App,
    project: Uuid,
    action: library::Action,
    retry: bool,
) -> Result<serde_json::Value> {
    let _permit = app
        .import_slot
        .acquire()
        .await
        .context("The import service is unavailable.")?;
    let inventory = library::scan(&app.store).await?;
    let mut state = app.inbox_state.lock().await;
    if retry {
        state.retry(project);
    }
    library::apply(
        &app.store, project, &inventory, action, MAX_IMPORT, &mut state,
    )
    .await
}

async fn import_tool(
    app: &App,
    project: Uuid,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    let paths = args["paths"]
        .as_array()
        .context("Pass paths from the studio's media inbox.")?;
    ensure!(
        (1..=32).contains(&paths.len()),
        "Import up to 32 files or inbox folders at once."
    );
    let mut paths = paths
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .context("Each path must be a file or folder.")
        })
        .collect::<Result<Vec<_>>>()?;
    paths.sort_unstable();
    paths.dedup();
    inbox_action(
        app,
        project,
        library::Action::Import {
            paths,
            offer_match: false,
        },
        true,
    )
    .await
}

fn install_sources(app: &App, project: Uuid, sources: &[Source]) -> Result<()> {
    app.store.update(|db| {
        let project = in_project(db, project)?;
        project.sources.extend(sources.iter().cloned());
        project.library_revision += 1;
        Ok(())
    })
}

async fn download_tool(
    app: &App,
    project: Uuid,
    args: &serde_json::Value,
) -> Result<serde_json::Value> {
    const MAX_MUSIC: usize = 200 * 1024 * 1024;
    let address = args["url"].as_str().context("The media URL is needed.")?;
    let license = args["licence"]
        .as_str()
        .filter(|license| !license.trim().is_empty())
        .context("Include the source’s media licence.")?;
    let source_page = public_web::address(
        args["source_page"]
            .as_str()
            .context("Include the HTTPS source page.")?,
    )?;
    let name = title(args["name"].as_str().unwrap_or("Downloaded media"))?;
    let _permit = app
        .import_slot
        .try_acquire()
        .context("Wait for the other import.")?;
    let mut response = public_web::fetch(address).await?;
    let mime = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    ensure!(
        !mime.starts_with("text/") && !mime.contains("html") && !mime.contains("json"),
        "That link is a web page. Use the direct audio file URL."
    );
    ensure!(
        response.content_length().unwrap_or(0) <= MAX_MUSIC as u64,
        "The audio exceeds the 200 MB download limit."
    );
    let work = media::temp(&app.store).await?;
    let result = async {
        let path = work.join("download.media");
        let mut file = tokio::fs::File::create(&path).await?;
        let mut count = 0;
        while let Some(bytes) = response.chunk().await? {
            count += bytes.len();
            ensure!(
                count <= MAX_MUSIC,
                "The audio exceeds the 200 MB download limit."
            );
            file.write_all(&bytes).await?;
        }
        drop(file);
        let (_, audio, duration) = media::probe(&path)
            .await
            .context("The link did not supply playable audio.")?;
        ensure!(
            audio && duration >= clock::TICKS_PER_SECOND,
            "The link did not supply at least one second of playable audio."
        );
        let mut asset = media::import(&app.store, &path, name).await?;
        asset.notes = format!(
            "Licence: {}\nSource: {}",
            license.chars().take(240).collect::<String>(),
            source_page.as_str().chars().take(500).collect::<String>()
        );
        if let Err(error) = install_sources(app, project, std::slice::from_ref(&asset)) {
            media::discard_import(&app.store, &asset).await;
            return Err(error);
        }
        Ok(json!({"asset":asset,"added":tools::asset_json(&asset),
            "source_page":source_page.as_str(),"licence":license,
            "note":"In the library now. Use add_clip to place it on the timeline."}))
    }
    .await;
    let _ = tokio::fs::remove_dir_all(work).await;
    result
}

fn queue_quick_clips(
    app: Shared,
    project_id: Uuid,
    args: &serde_json::Value,
    provider: Provider,
) -> Result<serde_json::Value> {
    let _edit = app.mcp_activity.channel(project_id).director_turn()?;
    ensure!(
        provider.name != Name::Local || !crate::provider::builtin_local(),
        "Connect ChatGPT or Claude to plan and review a render."
    );
    let request = args["request"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 3000)
        .context("Include the video request.")?
        .to_owned();
    let mut raw = args["plan"].clone();
    ensure!(raw.is_object(), "Supply a Quick Clips plan.");
    raw["action"] = json!("append");
    raw["message"] = json!("The sequence is ready.");
    let plan: crate::provider::Plan = serde_json::from_value(raw)?;
    plan.validate()?;
    let run = Run::new(project_id, vec!["Opened a Quick Clips run".into()]);
    let project = app.store.update(|db| {
        ensure!(
            !db.runs.iter().any(Run::pending),
            "Director is finishing another request."
        );
        let current = in_project(db, project_id)?;
        studio_history::remember_prompt(&mut current.prompt_history, &request, "video");
        current.remember_message(Message::new("you", request));
        let before = current.clone();
        let mut record = run.clone();
        record.studio_turn_mut().plan = Some((&plan).into());
        db.runs.push(record);
        if db.runs.len() > 120 {
            db.runs.remove(0);
        }
        Ok(before)
    })?;
    let id = run.id;
    app.playback.pause(&project);
    let preview = register_feed(&app, id);
    tokio::spawn(async move {
        let result = async {
            app.worker
                .model(project.sequence.render_model)
                .check()
                .await?;
            let work = media::temp(&app.store).await?;
            let result = Box::pin(render_plan(
                &app, &provider, &project, &plan, None, id, &work,
            ))
            .await;
            let _ = tokio::fs::remove_dir_all(work).await;
            result
        }
        .await;
        let (status, message) = match result {
            Ok(message) => ("done", message),
            Err(error) => ("failed", Message::new("director", error.to_string())),
        };
        let _ = record_run_reply(&app, project_id, id, status, message).await;
        finish_feed(&app, id, preview).await;
    });
    Ok(json!({"run_id":id,"status":"running"}))
}
