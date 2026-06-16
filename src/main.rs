use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use ammonia::Builder as AmmoniaBuilder;
use anyhow::{Context, anyhow};
use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use askama::Template;
use axum::{
    Form, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use pulldown_cmark::{Options, Parser, html};
use serde::{Deserialize, Serialize};
use sqlx::{
    FromRow, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use tower_http::{services::ServeDir, trace::TraceLayer};
use tower_sessions::{MemoryStore, Session, SessionManagerLayer};
use tracing::{error, info};
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::*;

const ADMIN_SESSION_KEY: &str = "is_admin";
const NOTE_GRANTS_KEY: &str = "granted_note_ids";
const FLASH_MESSAGE_KEY: &str = "flash_message";
const API_KEY_SECRET_KEY: &str = "api_key_created_secret";
const WEBAUTHN_STATE_TTL: Duration = Duration::from_secs(300);
const MAX_PENDING_WEBAUTHN_STATES: usize = 256;
const API_KEY_PREFIX_LEN: usize = 12;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "tabnotes=info,tower_http=info".into()),
        )
        .init();

    let config = Config::from_env()?;
    let bind_addr: SocketAddr = config
        .bind_addr
        .parse()
        .with_context(|| format!("invalid BIND_ADDR: {}", config.bind_addr))?;

    let connect_options = SqliteConnectOptions::from_str(&config.database_url)?
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(connect_options)
        .await
        .context("failed to connect to sqlite")?;

    init_db(&pool).await?;

    let webauthn = build_webauthn(&config)?;
    let state = Arc::new(AppState {
        pool,
        config,
        webauthn,
        pending_registrations: Mutex::new(HashMap::new()),
        pending_authentications: Mutex::new(HashMap::new()),
    });

    let app = app_router(state);

    info!("listening on {}", bind_addr);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

#[derive(Clone)]
struct Config {
    admin_username: String,
    admin_password: String,
    bind_addr: String,
    database_url: String,
    root_redirect_url: String,
    public_base_url: Url,
    passkey_rp_name: String,
    hide_footer: bool,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        let admin_username =
            std::env::var("ADMIN_USERNAME").context("missing ADMIN_USERNAME env var")?;
        let admin_password =
            std::env::var("ADMIN_PASSWORD").context("missing ADMIN_PASSWORD env var")?;
        let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
        let database_url =
            std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/tabnotes.db".into());
        let root_redirect_url =
            std::env::var("ROOT_REDIRECT_URL").unwrap_or_else(|_| "/dashboard".into());
        let public_base_url = Url::parse(
            &std::env::var("PUBLIC_BASE_URL")
                .context("missing PUBLIC_BASE_URL env var for passkeys and absolute links")?,
        )
        .context("invalid PUBLIC_BASE_URL")?;
        let passkey_rp_name =
            std::env::var("PASSKEY_RP_NAME").unwrap_or_else(|_| "TabNotes".into());
        let hide_footer = env_flag("HIDE_FOOTER");

        Ok(Self {
            admin_username,
            admin_password,
            bind_addr,
            database_url,
            root_redirect_url,
            public_base_url,
            passkey_rp_name,
            hide_footer,
        })
    }
}

struct AppState {
    pool: SqlitePool,
    config: Config,
    webauthn: Arc<Webauthn>,
    pending_registrations: Mutex<HashMap<String, PendingRegistration>>,
    pending_authentications: Mutex<HashMap<String, PendingAuthentication>>,
}

struct PendingRegistration {
    label: String,
    state: PasskeyRegistration,
    created_at: Instant,
}

struct PendingAuthentication {
    state: PasskeyAuthentication,
    created_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteVisibility {
    Admin,
    Public,
    Code,
}

impl NoteVisibility {
    fn as_str(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Public => "public",
            Self::Code => "code",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Admin => "🔒",
            Self::Public => "🌐",
            Self::Code => "🔑",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Admin => "Admin only",
            Self::Public => "Public",
            Self::Code => "Code protected",
        }
    }
}

impl TryFrom<&str> for NoteVisibility {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "admin" => Ok(Self::Admin),
            "public" => Ok(Self::Public),
            "code" => Ok(Self::Code),
            _ => Err(anyhow!("invalid visibility: {value}")),
        }
    }
}

#[derive(FromRow, Clone)]
struct NoteRecord {
    id: i64,
    title: String,
    slug: String,
    visibility: String,
    markdown: String,
    code_hash: Option<String>,
    #[allow(dead_code)]
    created_at: String,
    updated_at: String,
}

impl NoteRecord {
    fn visibility_enum(&self) -> anyhow::Result<NoteVisibility> {
        NoteVisibility::try_from(self.visibility.as_str())
    }
}

#[derive(FromRow)]
struct PasskeyRecord {
    id: i64,
    label: String,
    credential_json: String,
    created_at: String,
}

#[derive(FromRow, Clone)]
struct ApiKeyRecord {
    id: i64,
    label: String,
    key_prefix: String,
    key_hash: String,
    created_at: String,
    last_used_at: Option<String>,
}

#[derive(Clone)]
struct DashboardNote {
    id: i64,
    title: String,
    slug: String,
    visibility_icon: String,
    visibility_label: String,
    updated_at: String,
}

#[derive(Clone)]
struct DashboardPasskey {
    id: i64,
    label: String,
    created_at: String,
}

#[derive(Clone)]
struct DashboardApiKey {
    id: i64,
    label: String,
    key_prefix: String,
    created_at: String,
    last_used_at: String,
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    page_title: String,
    body_class: String,
    is_admin: bool,
    noindex: bool,
    show_footer: bool,
    error_message: String,
    next_url: String,
    passkeys_available: bool,
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    page_title: String,
    body_class: String,
    is_admin: bool,
    noindex: bool,
    show_footer: bool,
    notes: Vec<DashboardNote>,
    passkeys: Vec<DashboardPasskey>,
    api_keys: Vec<DashboardApiKey>,
    passkey_supported: bool,
    flash_message: String,
    api_key_secret: String,
}

#[derive(Template)]
#[template(path = "editor.html")]
struct EditorTemplate {
    page_title: String,
    body_class: String,
    is_admin: bool,
    noindex: bool,
    show_footer: bool,
    form_action: String,
    submit_label: String,
    error_message: String,
    title_value: String,
    slug_value: String,
    markdown_value: String,
    visibility_value: String,
    access_code_placeholder: String,
    preview_html: String,
    share_url: String,
    delete_url: String,
    editor_mode: String,
}

#[derive(Template)]
#[template(path = "note.html")]
struct NoteTemplate {
    page_title: String,
    body_class: String,
    is_admin: bool,
    noindex: bool,
    show_footer: bool,
    note_title: String,
    note_html: String,
    updated_at: String,
    edit_url: String,
    delete_url: String,
}

#[derive(Template)]
#[template(path = "code_gate.html")]
struct CodeGateTemplate {
    page_title: String,
    body_class: String,
    is_admin: bool,
    noindex: bool,
    show_footer: bool,
    error_message: String,
    form_action: String,
}

#[derive(Template)]
#[template(path = "not_found.html")]
struct NotFoundTemplate {
    page_title: String,
    body_class: String,
    is_admin: bool,
    noindex: bool,
    show_footer: bool,
}

#[derive(Deserialize)]
struct LoginQuery {
    next: Option<String>,
}

#[derive(Deserialize)]
struct LoginFormData {
    username: String,
    password: String,
    next: Option<String>,
}

#[derive(Deserialize)]
struct NoteFormData {
    title: String,
    slug: String,
    markdown: String,
    visibility: String,
    access_code: String,
}

#[derive(Deserialize)]
struct ApiKeyFormData {
    label: String,
}

#[derive(Deserialize)]
struct UnlockCodeForm {
    access_code: String,
}

#[derive(Deserialize)]
struct PreviewRequest {
    markdown: String,
}

#[derive(Serialize)]
struct PreviewResponse {
    html: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasskeyStartRegistrationRequest {
    label: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PasskeyStartResponse {
    state_id: String,
    options: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasskeyFinishRegistrationRequest {
    state_id: String,
    credential: RegisterPublicKeyCredential,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasskeyFinishAuthenticationRequest {
    state_id: String,
    credential: PublicKeyCredential,
}

#[derive(Serialize)]
struct ApiOk {
    ok: bool,
}

#[derive(Deserialize)]
struct ApiNotePayload {
    title: String,
    slug: Option<String>,
    markdown: String,
    visibility: String,
    access_code: Option<String>,
}

#[derive(Serialize)]
struct ApiNoteResponse {
    id: i64,
    title: String,
    slug: String,
    visibility: String,
    markdown: String,
    created_at: String,
    updated_at: String,
    share_url: String,
}

#[derive(Serialize)]
struct ApiErrorBody {
    error: String,
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, message)
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, value.to_string())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(value: sqlx::Error) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, value.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ApiErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

async fn init_db(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS notes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            slug TEXT NOT NULL UNIQUE,
            visibility TEXT NOT NULL CHECK (visibility IN ('admin', 'public', 'code')),
            markdown TEXT NOT NULL,
            code_hash TEXT,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS passkeys (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            label TEXT NOT NULL,
            credential_json TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS api_keys (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            label TEXT NOT NULL,
            key_prefix TEXT NOT NULL UNIQUE,
            key_hash TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            last_used_at TEXT
        )
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}

fn build_webauthn(config: &Config) -> anyhow::Result<Arc<Webauthn>> {
    let rp_id = config
        .public_base_url
        .host_str()
        .context("PUBLIC_BASE_URL must include a hostname")?;

    let webauthn = WebauthnBuilder::new(rp_id, &config.public_base_url)?
        .rp_name(&config.passkey_rp_name)
        .build()?;

    Ok(Arc::new(webauthn))
}

fn app_router(state: Arc<AppState>) -> Router {
    let session_store = MemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(state.config.public_base_url.scheme() == "https")
        .with_same_site(tower_sessions::cookie::SameSite::Lax);

    Router::new()
        .route("/", get(root_redirect))
        .route("/login", get(login_page).post(login_submit))
        .route("/logout", post(logout))
        .route("/dashboard", get(dashboard))
        .route("/admin/notes/new", get(new_note_page).post(create_note))
        .route(
            "/admin/notes/{id}/edit",
            get(edit_note_page).post(update_note),
        )
        .route("/admin/notes/{id}/delete", post(delete_note))
        .route("/admin/preview", post(markdown_preview))
        .route("/admin/passkeys/start", post(start_passkey_registration))
        .route("/admin/passkeys/finish", post(finish_passkey_registration))
        .route("/admin/passkeys/{id}/delete", post(delete_passkey))
        .route("/admin/api-keys", post(create_api_key))
        .route("/admin/api-keys/{id}/delete", post(delete_api_key))
        .route("/auth/passkey/start", post(start_passkey_authentication))
        .route("/auth/passkey/finish", post(finish_passkey_authentication))
        .route("/notes/{slug}", get(view_note))
        .route("/notes/{slug}/code", post(unlock_code_note))
        .route("/api/notes", get(api_list_notes).post(api_create_note))
        .route(
            "/api/notes/{id}",
            get(api_get_note)
                .put(api_update_note)
                .delete(api_delete_note),
        )
        .fallback(get(not_found_route))
        .nest_service("/static", ServeDir::new("static"))
        .layer(session_layer)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn root_redirect(State(state): State<Arc<AppState>>) -> Redirect {
    Redirect::to(&state.config.root_redirect_url)
}

async fn not_found_route() -> AppError {
    AppError::not_found()
}

async fn login_page(
    State(state): State<Arc<AppState>>,
    session: Session,
    Query(query): Query<LoginQuery>,
) -> AppResult<Response> {
    if is_admin(&session).await? {
        return Ok(Redirect::to("/dashboard").into_response());
    }

    let passkeys_available = !load_passkeys(&state.pool).await?.is_empty();
    let template = LoginTemplate {
        page_title: "Login".into(),
        body_class: "auth-page".into(),
        is_admin: false,
        noindex: true,
        show_footer: !state.config.hide_footer,
        error_message: String::new(),
        next_url: query.next.unwrap_or_else(|| "/dashboard".into()),
        passkeys_available,
    };

    render_page(&template, true)
}

async fn login_submit(
    State(state): State<Arc<AppState>>,
    session: Session,
    Form(form): Form<LoginFormData>,
) -> AppResult<Response> {
    let next = sanitize_next(form.next.as_deref());
    let username_ok = state
        .config
        .admin_username
        .as_bytes()
        .ct_eq(form.username.as_bytes())
        .into();
    let password_ok = state
        .config
        .admin_password
        .as_bytes()
        .ct_eq(form.password.as_bytes())
        .into();

    if username_ok && password_ok {
        session.insert(ADMIN_SESSION_KEY, true).await?;
        return Ok(Redirect::to(&next).into_response());
    }

    let passkeys_available = !load_passkeys(&state.pool).await?.is_empty();
    let template = LoginTemplate {
        page_title: "Login".into(),
        body_class: "auth-page".into(),
        is_admin: false,
        noindex: true,
        show_footer: !state.config.hide_footer,
        error_message: "Invalid credentials.".into(),
        next_url: next,
        passkeys_available,
    };
    render_page(&template, true)
}

async fn logout(session: Session) -> AppResult<Response> {
    session.flush().await?;
    Ok(Redirect::to("/login").into_response())
}

async fn dashboard(State(state): State<Arc<AppState>>, session: Session) -> AppResult<Response> {
    require_admin(&session, "/dashboard").await?;

    let notes = sqlx::query_as::<_, NoteRecord>(
        "SELECT id, title, slug, visibility, markdown, code_hash, created_at, updated_at FROM notes ORDER BY updated_at DESC",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|note| {
        let (visibility_icon, visibility_label) = note
            .visibility_enum()
            .map(|visibility| (visibility.icon().to_string(), visibility.label().to_string()))
            .unwrap_or_else(|_| ("❓".into(), "Unknown".into()));

        DashboardNote {
            id: note.id,
            title: note.title.clone(),
            slug: note.slug.clone(),
            visibility_icon,
            visibility_label,
            updated_at: note.updated_at,
        }
    })
    .collect();

    let passkeys = sqlx::query_as::<_, PasskeyRecord>(
        "SELECT id, label, credential_json, created_at FROM passkeys ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|row| {
        let _ = row.credential_json;
        DashboardPasskey {
            id: row.id,
            label: row.label,
            created_at: row.created_at,
        }
    })
    .collect();

    let api_keys = sqlx::query_as::<_, ApiKeyRecord>(
        "SELECT id, label, key_prefix, key_hash, created_at, last_used_at FROM api_keys ORDER BY created_at DESC",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|row| {
        let _ = row.key_hash;
        DashboardApiKey {
            id: row.id,
            label: row.label,
            key_prefix: row.key_prefix,
            created_at: row.created_at,
            last_used_at: row.last_used_at.unwrap_or_else(|| "Never".into()),
        }
    })
    .collect();

    let flash_message = session
        .remove::<String>(FLASH_MESSAGE_KEY)
        .await?
        .unwrap_or_default();
    let api_key_secret = session
        .remove::<String>(API_KEY_SECRET_KEY)
        .await?
        .unwrap_or_default();

    let template = DashboardTemplate {
        page_title: "Dashboard".into(),
        body_class: "dashboard-page".into(),
        is_admin: true,
        noindex: true,
        show_footer: !state.config.hide_footer,
        notes,
        passkeys,
        api_keys,
        passkey_supported: true,
        flash_message,
        api_key_secret,
    };

    render_page(&template, true)
}

async fn new_note_page(
    State(state): State<Arc<AppState>>,
    session: Session,
) -> AppResult<Response> {
    require_admin(&session, "/admin/notes/new").await?;

    let template = build_editor_template(
        &state,
        None,
        NoteFormData {
            title: String::new(),
            slug: String::new(),
            markdown: String::new(),
            visibility: "admin".into(),
            access_code: String::new(),
        },
        String::new(),
    );
    render_page(&template, true)
}

async fn create_note(
    State(state): State<Arc<AppState>>,
    session: Session,
    Form(form): Form<NoteFormData>,
) -> AppResult<Response> {
    require_admin(&session, "/admin/notes/new").await?;

    match validate_note_form(None, &form).await {
        Ok(validated) => {
            sqlx::query(
                r#"
                INSERT INTO notes (title, slug, visibility, markdown, code_hash, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP)
                "#,
            )
            .bind(validated.title)
            .bind(validated.slug)
            .bind(validated.visibility.as_str())
            .bind(validated.markdown)
            .bind(validated.code_hash)
            .execute(&state.pool)
            .await
            .map_err(map_sqlite_error)?;

            Ok(Redirect::to("/dashboard").into_response())
        }
        Err(message) => {
            let template = build_editor_template(&state, None, form, message);
            render_page(&template, true)
        }
    }
}

async fn edit_note_page(
    State(state): State<Arc<AppState>>,
    session: Session,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_admin(&session, &format!("/admin/notes/{id}/edit")).await?;
    let note = load_note_by_id(&state.pool, id).await?;

    let template = build_editor_template(
        &state,
        Some(&note),
        NoteFormData {
            title: note.title.clone(),
            slug: note.slug.clone(),
            markdown: note.markdown.clone(),
            visibility: note.visibility.clone(),
            access_code: String::new(),
        },
        String::new(),
    );

    render_page(&template, true)
}

async fn update_note(
    State(state): State<Arc<AppState>>,
    session: Session,
    Path(id): Path<i64>,
    Form(form): Form<NoteFormData>,
) -> AppResult<Response> {
    require_admin(&session, &format!("/admin/notes/{id}/edit")).await?;
    let existing = load_note_by_id(&state.pool, id).await?;

    match validate_note_form(Some(&existing), &form).await {
        Ok(validated) => {
            sqlx::query(
                r#"
                UPDATE notes
                SET title = ?1, slug = ?2, visibility = ?3, markdown = ?4, code_hash = ?5, updated_at = CURRENT_TIMESTAMP
                WHERE id = ?6
                "#,
            )
            .bind(validated.title)
            .bind(validated.slug)
            .bind(validated.visibility.as_str())
            .bind(validated.markdown)
            .bind(validated.code_hash)
            .bind(id)
            .execute(&state.pool)
            .await
            .map_err(map_sqlite_error)?;

            Ok(Redirect::to("/dashboard").into_response())
        }
        Err(message) => {
            let template = build_editor_template(&state, Some(&existing), form, message);
            render_page(&template, true)
        }
    }
}

async fn delete_note(
    State(state): State<Arc<AppState>>,
    session: Session,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_admin(&session, "/dashboard").await?;
    let note = load_note_by_id(&state.pool, id).await?;

    sqlx::query("DELETE FROM notes WHERE id = ?1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    set_flash_message(&session, format!("Deleted note: {}", note.title)).await?;
    Ok(Redirect::to("/dashboard").into_response())
}

async fn create_api_key(
    State(state): State<Arc<AppState>>,
    session: Session,
    Form(form): Form<ApiKeyFormData>,
) -> AppResult<Response> {
    require_admin(&session, "/dashboard").await?;

    let label = form.label.trim();
    if label.is_empty() {
        set_flash_message(&session, "API key label is required.".to_string()).await?;
        return Ok(Redirect::to("/dashboard").into_response());
    }

    let (key_prefix, raw_key) = generate_api_key_value();
    let key_hash = hash_secret(&raw_key)?;

    sqlx::query("INSERT INTO api_keys (label, key_prefix, key_hash) VALUES (?1, ?2, ?3)")
        .bind(label)
        .bind(&key_prefix)
        .bind(key_hash)
        .execute(&state.pool)
        .await?;

    set_flash_message(&session, format!("Created API key: {label}")).await?;
    session.insert(API_KEY_SECRET_KEY, raw_key).await?;
    Ok(Redirect::to("/dashboard").into_response())
}

async fn delete_passkey(
    State(state): State<Arc<AppState>>,
    session: Session,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_admin(&session, "/dashboard").await?;

    let passkey = load_passkey_by_id(&state.pool, id).await?;
    sqlx::query("DELETE FROM passkeys WHERE id = ?1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    set_flash_message(&session, format!("Deleted passkey: {}", passkey.label)).await?;
    Ok(Redirect::to("/dashboard").into_response())
}

async fn delete_api_key(
    State(state): State<Arc<AppState>>,
    session: Session,
    Path(id): Path<i64>,
) -> AppResult<Response> {
    require_admin(&session, "/dashboard").await?;

    let api_key = load_api_key_by_id(&state.pool, id).await?;
    sqlx::query("DELETE FROM api_keys WHERE id = ?1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    set_flash_message(&session, format!("Revoked API key: {}", api_key.label)).await?;
    Ok(Redirect::to("/dashboard").into_response())
}

async fn markdown_preview(
    session: Session,
    Json(payload): Json<PreviewRequest>,
) -> AppResult<Json<PreviewResponse>> {
    require_admin(&session, "/admin/preview").await?;
    Ok(Json(PreviewResponse {
        html: render_markdown(&payload.markdown),
    }))
}

async fn start_passkey_registration(
    State(state): State<Arc<AppState>>,
    session: Session,
    Json(payload): Json<PasskeyStartRegistrationRequest>,
) -> AppResult<Json<PasskeyStartResponse>> {
    require_admin(&session, "/dashboard").await?;

    let label = payload.label.trim();
    if label.is_empty() {
        return Err(anyhow!("passkey label is required").into());
    }

    let user_id = Uuid::new_v4();
    let (options, reg_state) = state.webauthn.start_passkey_registration(
        user_id,
        &state.config.admin_username,
        &state.config.admin_username,
        None,
    )?;

    let state_id = Uuid::new_v4().to_string();
    let mut pending_registrations = state.pending_registrations.lock().await;
    cleanup_pending_states(&mut pending_registrations);
    ensure_pending_capacity(&pending_registrations)?;
    pending_registrations.insert(
        state_id.clone(),
        PendingRegistration {
            label: label.to_string(),
            state: reg_state,
            created_at: Instant::now(),
        },
    );

    Ok(Json(PasskeyStartResponse {
        state_id,
        options: serde_json::to_value(options)?,
    }))
}

async fn finish_passkey_registration(
    State(state): State<Arc<AppState>>,
    session: Session,
    Json(payload): Json<PasskeyFinishRegistrationRequest>,
) -> AppResult<Json<ApiOk>> {
    require_admin(&session, "/dashboard").await?;

    let pending = state
        .pending_registrations
        .lock()
        .await
        .remove(&payload.state_id)
        .ok_or_else(|| anyhow!("passkey registration session expired"))?;
    if pending.created_at.elapsed() > WEBAUTHN_STATE_TTL {
        return Err(anyhow!("passkey registration session expired").into());
    }

    let passkey = state
        .webauthn
        .finish_passkey_registration(&payload.credential, &pending.state)?;

    sqlx::query("INSERT INTO passkeys (label, credential_json) VALUES (?1, ?2)")
        .bind(pending.label)
        .bind(serde_json::to_string(&passkey)?)
        .execute(&state.pool)
        .await?;

    Ok(Json(ApiOk { ok: true }))
}

async fn start_passkey_authentication(
    State(state): State<Arc<AppState>>,
) -> AppResult<Json<PasskeyStartResponse>> {
    let passkeys = load_passkeys(&state.pool).await?;
    if passkeys.is_empty() {
        return Err(anyhow!("no passkeys are registered").into());
    }

    let (options, auth_state) = state.webauthn.start_passkey_authentication(&passkeys)?;
    let state_id = Uuid::new_v4().to_string();
    let mut pending_authentications = state.pending_authentications.lock().await;
    cleanup_pending_states(&mut pending_authentications);
    ensure_pending_capacity(&pending_authentications)?;
    pending_authentications.insert(
        state_id.clone(),
        PendingAuthentication {
            state: auth_state,
            created_at: Instant::now(),
        },
    );

    Ok(Json(PasskeyStartResponse {
        state_id,
        options: serde_json::to_value(options)?,
    }))
}

async fn finish_passkey_authentication(
    State(state): State<Arc<AppState>>,
    session: Session,
    Json(payload): Json<PasskeyFinishAuthenticationRequest>,
) -> AppResult<Json<ApiOk>> {
    let auth_state = state
        .pending_authentications
        .lock()
        .await
        .remove(&payload.state_id)
        .ok_or_else(|| anyhow!("passkey authentication session expired"))?;
    if auth_state.created_at.elapsed() > WEBAUTHN_STATE_TTL {
        return Err(anyhow!("passkey authentication session expired").into());
    }

    let auth_result = state
        .webauthn
        .finish_passkey_authentication(&payload.credential, &auth_state.state)?;

    update_stored_passkey(&state.pool, &auth_result).await?;

    session.insert(ADMIN_SESSION_KEY, true).await?;
    Ok(Json(ApiOk { ok: true }))
}

async fn view_note(
    State(state): State<Arc<AppState>>,
    session: Session,
    Path(slug): Path<String>,
) -> AppResult<Response> {
    let note = load_note_by_slug(&state.pool, &slug).await?;
    let admin = is_admin(&session).await?;
    match note.visibility_enum()? {
        NoteVisibility::Admin => {
            if !admin {
                return Ok(
                    Redirect::to(&login_redirect_for(&format!("/notes/{}", note.slug)))
                        .into_response(),
                );
            }
        }
        NoteVisibility::Public => {}
        NoteVisibility::Code => {
            let grants = session
                .get::<HashSet<i64>>(NOTE_GRANTS_KEY)
                .await?
                .unwrap_or_default();
            if !grants.contains(&note.id) && !admin {
                let template = CodeGateTemplate {
                    page_title: format!("Unlock {}", note.title),
                    body_class: "reader-page".into(),
                    is_admin: false,
                    noindex: true,
                    show_footer: !state.config.hide_footer,
                    error_message: String::new(),
                    form_action: format!("/notes/{}/code", note.slug),
                };
                return render_page(&template, true);
            }
        }
    }

    let template = NoteTemplate {
        page_title: note.title.clone(),
        body_class: "reader-page".into(),
        is_admin: admin,
        noindex: true,
        show_footer: !state.config.hide_footer,
        note_title: note.title.clone(),
        note_html: render_markdown(&note.markdown),
        updated_at: note.updated_at.clone(),
        edit_url: format!("/admin/notes/{}/edit", note.id),
        delete_url: format!("/admin/notes/{}/delete", note.id),
    };

    render_page(&template, true)
}

async fn unlock_code_note(
    State(state): State<Arc<AppState>>,
    session: Session,
    Path(slug): Path<String>,
    Form(form): Form<UnlockCodeForm>,
) -> AppResult<Response> {
    let note = load_note_by_slug(&state.pool, &slug).await?;
    if note.visibility_enum()? != NoteVisibility::Code {
        return Ok(Redirect::to(&format!("/notes/{}", note.slug)).into_response());
    }

    let Some(code_hash) = note.code_hash.as_deref() else {
        return Err(anyhow!("code-protected note is missing a code hash").into());
    };
    let password_hash = PasswordHash::new(code_hash).map_err(|err| anyhow!(err.to_string()))?;

    if Argon2::default()
        .verify_password(form.access_code.as_bytes(), &password_hash)
        .is_ok()
    {
        let mut grants = session
            .get::<HashSet<i64>>(NOTE_GRANTS_KEY)
            .await?
            .unwrap_or_default();
        grants.insert(note.id);
        session.insert(NOTE_GRANTS_KEY, grants).await?;
        return Ok(Redirect::to(&format!("/notes/{}", note.slug)).into_response());
    }

    let template = CodeGateTemplate {
        page_title: format!("Unlock {}", note.title),
        body_class: "reader-page".into(),
        is_admin: false,
        noindex: true,
        show_footer: !state.config.hide_footer,
        error_message: "Invalid access code.".into(),
        form_action: format!("/notes/{}/code", note.slug),
    };
    render_page(&template, true)
}

fn build_editor_template(
    state: &AppState,
    note: Option<&NoteRecord>,
    form: NoteFormData,
    error_message: String,
) -> EditorTemplate {
    let slug = if form.slug.trim().is_empty() {
        slugify(&form.title)
    } else {
        slugify(&form.slug)
    };
    let share_url = if slug.is_empty() {
        String::new()
    } else {
        state
            .config
            .public_base_url
            .join(&format!("notes/{slug}"))
            .map(|url| url.to_string())
            .unwrap_or_default()
    };

    EditorTemplate {
        page_title: match note {
            Some(existing) => format!("Edit {}", existing.title),
            None => "New Note".into(),
        },
        body_class: "editor-page".into(),
        is_admin: true,
        noindex: true,
        show_footer: !state.config.hide_footer,
        form_action: match note {
            Some(existing) => format!("/admin/notes/{}/edit", existing.id),
            None => "/admin/notes/new".into(),
        },
        submit_label: match note {
            Some(_) => "Save changes".into(),
            None => "Create note".into(),
        },
        error_message,
        title_value: form.title,
        slug_value: slug,
        markdown_value: form.markdown.clone(),
        visibility_value: form.visibility,
        access_code_placeholder: form.access_code,
        preview_html: render_markdown(&form.markdown),
        share_url,
        delete_url: note
            .map(|existing| format!("/admin/notes/{}/delete", existing.id))
            .unwrap_or_default(),
        editor_mode: "split".into(),
    }
}

async fn load_note_by_id(pool: &SqlitePool, id: i64) -> AppResult<NoteRecord> {
    sqlx::query_as::<_, NoteRecord>(
        "SELECT id, title, slug, visibility, markdown, code_hash, created_at, updated_at FROM notes WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(AppError::not_found)
}

async fn load_note_by_slug(pool: &SqlitePool, slug: &str) -> AppResult<NoteRecord> {
    sqlx::query_as::<_, NoteRecord>(
        "SELECT id, title, slug, visibility, markdown, code_hash, created_at, updated_at FROM notes WHERE slug = ?1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await?
    .ok_or_else(AppError::not_found)
}

async fn load_passkeys(pool: &SqlitePool) -> AppResult<Vec<Passkey>> {
    let rows = sqlx::query_as::<_, PasskeyRecord>(
        "SELECT id, label, credential_json, created_at FROM passkeys ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            let _ = row.id;
            let _ = row.label;
            let _ = row.created_at;
            serde_json::from_str::<Passkey>(&row.credential_json).map_err(AppError::from)
        })
        .collect()
}

async fn load_passkey_by_id(pool: &SqlitePool, id: i64) -> AppResult<PasskeyRecord> {
    sqlx::query_as::<_, PasskeyRecord>(
        "SELECT id, label, credential_json, created_at FROM passkeys WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(AppError::not_found)
}

async fn update_stored_passkey(
    pool: &SqlitePool,
    auth_result: &AuthenticationResult,
) -> AppResult<()> {
    let rows = sqlx::query_as::<_, PasskeyRecord>(
        "SELECT id, label, credential_json, created_at FROM passkeys ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await?;

    for row in rows {
        let mut passkey: Passkey = serde_json::from_str(&row.credential_json)?;
        match passkey.update_credential(auth_result) {
            Some(true) => {
                sqlx::query("UPDATE passkeys SET credential_json = ?1 WHERE id = ?2")
                    .bind(serde_json::to_string(&passkey)?)
                    .bind(row.id)
                    .execute(pool)
                    .await?;
                return Ok(());
            }
            Some(false) => return Ok(()),
            None => continue,
        }
    }

    Err(anyhow!("authenticated passkey was not found in storage").into())
}

async fn load_api_key_by_id(pool: &SqlitePool, id: i64) -> AppResult<ApiKeyRecord> {
    sqlx::query_as::<_, ApiKeyRecord>(
        "SELECT id, label, key_prefix, key_hash, created_at, last_used_at FROM api_keys WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(AppError::not_found)
}

async fn load_note_by_id_api(pool: &SqlitePool, id: i64) -> ApiResult<NoteRecord> {
    sqlx::query_as::<_, NoteRecord>(
        "SELECT id, title, slug, visibility, markdown, code_hash, created_at, updated_at FROM notes WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| ApiError::not_found("Note not found."))
}

async fn authenticate_api_key(state: &AppState, headers: &HeaderMap) -> ApiResult<ApiKeyRecord> {
    let raw_key =
        extract_api_key(headers).ok_or_else(|| ApiError::unauthorized("Missing API key."))?;
    let key_prefix =
        parse_api_key_prefix(raw_key).ok_or_else(|| ApiError::unauthorized("Invalid API key."))?;

    let api_key = sqlx::query_as::<_, ApiKeyRecord>(
        "SELECT id, label, key_prefix, key_hash, created_at, last_used_at FROM api_keys WHERE key_prefix = ?1",
    )
    .bind(key_prefix)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| ApiError::unauthorized("Invalid API key."))?;

    let password_hash = PasswordHash::new(&api_key.key_hash)
        .map_err(|_| ApiError::unauthorized("Invalid API key."))?;
    if Argon2::default()
        .verify_password(raw_key.as_bytes(), &password_hash)
        .is_err()
    {
        return Err(ApiError::unauthorized("Invalid API key."));
    }

    sqlx::query("UPDATE api_keys SET last_used_at = CURRENT_TIMESTAMP WHERE id = ?1")
        .bind(api_key.id)
        .execute(&state.pool)
        .await?;

    Ok(api_key)
}

fn extract_api_key(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty())
        .map(str::trim)
        .or_else(|| {
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
}

fn parse_api_key_prefix(raw_key: &str) -> Option<&str> {
    let rest = raw_key.strip_prefix("tn_")?;
    let (prefix, secret) = rest.split_once('_')?;
    if prefix.len() == API_KEY_PREFIX_LEN && !secret.trim().is_empty() {
        Some(prefix)
    } else {
        None
    }
}

fn generate_api_key_value() -> (String, String) {
    let prefix_seed = Uuid::new_v4().simple().to_string();
    let key_prefix = prefix_seed[..API_KEY_PREFIX_LEN].to_string();
    let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let raw_key = format!("tn_{}_{}", key_prefix, secret);
    (key_prefix, raw_key)
}

async fn set_flash_message(session: &Session, message: String) -> AppResult<()> {
    session.insert(FLASH_MESSAGE_KEY, message).await?;
    Ok(())
}

fn map_api_sqlite_error(error: sqlx::Error) -> ApiError {
    match &error {
        sqlx::Error::Database(db_error)
            if db_error
                .message()
                .contains("UNIQUE constraint failed: notes.slug") =>
        {
            ApiError::new(StatusCode::CONFLICT, "That slug is already in use.")
        }
        _ => ApiError::from(error),
    }
}

fn api_note_payload_to_form(payload: ApiNotePayload) -> NoteFormData {
    NoteFormData {
        title: payload.title,
        slug: payload.slug.unwrap_or_default(),
        markdown: payload.markdown,
        visibility: payload.visibility,
        access_code: payload.access_code.unwrap_or_default(),
    }
}

fn share_url_for_slug(state: &AppState, slug: &str) -> String {
    state
        .config
        .public_base_url
        .join(&format!("notes/{slug}"))
        .map(|url| url.to_string())
        .unwrap_or_default()
}

fn note_to_api_response(state: &AppState, note: NoteRecord) -> ApiNoteResponse {
    ApiNoteResponse {
        id: note.id,
        title: note.title,
        slug: note.slug.clone(),
        visibility: note.visibility,
        markdown: note.markdown,
        created_at: note.created_at,
        updated_at: note.updated_at,
        share_url: share_url_for_slug(state, &note.slug),
    }
}

async fn api_list_notes(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<ApiNoteResponse>>> {
    let _api_key = authenticate_api_key(&state, &headers).await?;
    let notes = sqlx::query_as::<_, NoteRecord>(
        "SELECT id, title, slug, visibility, markdown, code_hash, created_at, updated_at FROM notes ORDER BY updated_at DESC",
    )
    .fetch_all(&state.pool)
    .await?
    .into_iter()
    .map(|note| note_to_api_response(&state, note))
    .collect();

    Ok(Json(notes))
}

async fn api_get_note(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<ApiNoteResponse>> {
    let _api_key = authenticate_api_key(&state, &headers).await?;
    let note = load_note_by_id_api(&state.pool, id).await?;
    Ok(Json(note_to_api_response(&state, note)))
}

async fn api_create_note(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<ApiNotePayload>,
) -> ApiResult<(StatusCode, Json<ApiNoteResponse>)> {
    let _api_key = authenticate_api_key(&state, &headers).await?;
    let form = api_note_payload_to_form(payload);
    let validated = validate_note_form(None, &form)
        .await
        .map_err(ApiError::bad_request)?;

    let result = sqlx::query(
        r#"
        INSERT INTO notes (title, slug, visibility, markdown, code_hash, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP)
        "#,
    )
    .bind(validated.title)
    .bind(validated.slug)
    .bind(validated.visibility.as_str())
    .bind(validated.markdown)
    .bind(validated.code_hash)
    .execute(&state.pool)
    .await
    .map_err(map_api_sqlite_error)?;

    let note = load_note_by_id_api(&state.pool, result.last_insert_rowid()).await?;
    Ok((
        StatusCode::CREATED,
        Json(note_to_api_response(&state, note)),
    ))
}

async fn api_update_note(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(payload): Json<ApiNotePayload>,
) -> ApiResult<Json<ApiNoteResponse>> {
    let _api_key = authenticate_api_key(&state, &headers).await?;
    let existing = load_note_by_id_api(&state.pool, id).await?;
    let form = api_note_payload_to_form(payload);
    let validated = validate_note_form(Some(&existing), &form)
        .await
        .map_err(ApiError::bad_request)?;

    sqlx::query(
        r#"
        UPDATE notes
        SET title = ?1, slug = ?2, visibility = ?3, markdown = ?4, code_hash = ?5, updated_at = CURRENT_TIMESTAMP
        WHERE id = ?6
        "#,
    )
    .bind(validated.title)
    .bind(validated.slug)
    .bind(validated.visibility.as_str())
    .bind(validated.markdown)
    .bind(validated.code_hash)
    .bind(id)
    .execute(&state.pool)
    .await
    .map_err(map_api_sqlite_error)?;

    let note = load_note_by_id_api(&state.pool, id).await?;
    Ok(Json(note_to_api_response(&state, note)))
}

async fn api_delete_note(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let _api_key = authenticate_api_key(&state, &headers).await?;
    let _note = load_note_by_id_api(&state.pool, id).await?;

    sqlx::query("DELETE FROM notes WHERE id = ?1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}

async fn is_admin(session: &Session) -> AppResult<bool> {
    Ok(session
        .get::<bool>(ADMIN_SESSION_KEY)
        .await?
        .unwrap_or(false))
}

async fn require_admin(session: &Session, next: &str) -> AppResult<()> {
    if is_admin(session).await? {
        return Ok(());
    }
    Err(AppError::redirect(login_redirect_for(next)))
}

fn login_redirect_for(next: &str) -> String {
    format!("/login?next={}", urlencoding::encode(next))
}

fn sanitize_next(next: Option<&str>) -> String {
    match next {
        Some(path) if path.starts_with('/') && !path.starts_with("//") => path.to_string(),
        _ => "/dashboard".into(),
    }
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn cleanup_pending_states<T>(states: &mut HashMap<String, T>)
where
    T: PendingState,
{
    states.retain(|_, state| state.created_at().elapsed() <= WEBAUTHN_STATE_TTL);
}

fn ensure_pending_capacity<T>(states: &HashMap<String, T>) -> AppResult<()> {
    if states.len() >= MAX_PENDING_WEBAUTHN_STATES {
        return Err(anyhow!("too many pending authentication attempts, try again shortly").into());
    }
    Ok(())
}

trait PendingState {
    fn created_at(&self) -> Instant;
}

impl PendingState for PendingRegistration {
    fn created_at(&self) -> Instant {
        self.created_at
    }
}

impl PendingState for PendingAuthentication {
    fn created_at(&self) -> Instant {
        self.created_at
    }
}

fn render_markdown(markdown: &str) -> String {
    let parser = Parser::new_ext(
        markdown,
        Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES,
    );
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    AmmoniaBuilder::default()
        .add_tags([
            "h1", "h2", "img", "table", "thead", "tbody", "tr", "th", "td",
        ])
        .clean(&html_output)
        .to_string()
}

fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in input.trim().chars().flat_map(|c| c.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        format!("note-{}", Uuid::new_v4().simple())
    } else {
        slug
    }
}

async fn validate_note_form(
    existing: Option<&NoteRecord>,
    form: &NoteFormData,
) -> Result<ValidatedNoteForm, String> {
    let title = form.title.trim();
    if title.is_empty() {
        return Err("Title is required.".into());
    }

    let markdown = form.markdown.trim_end().to_string();
    let slug = slugify(if form.slug.trim().is_empty() {
        title
    } else {
        form.slug.trim()
    });
    let visibility = NoteVisibility::try_from(form.visibility.as_str())
        .map_err(|_| "Select a valid visibility.".to_string())?;

    let code_hash = match visibility {
        NoteVisibility::Code => {
            if !form.access_code.trim().is_empty() {
                Some(hash_secret(form.access_code.trim()).map_err(|e| e.to_string())?)
            } else if let Some(existing) = existing {
                existing.code_hash.clone().or(None)
            } else {
                None
            }
        }
        _ => None,
    };

    if visibility == NoteVisibility::Code && code_hash.is_none() {
        return Err("Code-protected notes require an access code.".into());
    }

    Ok(ValidatedNoteForm {
        title: title.to_string(),
        slug,
        markdown,
        visibility,
        code_hash,
    })
}

fn hash_secret(secret: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(secret.as_bytes(), &salt)
        .map_err(|err| anyhow!(err.to_string()))?
        .to_string())
}

fn map_sqlite_error(error: sqlx::Error) -> AppError {
    match &error {
        sqlx::Error::Database(db_error)
            if db_error
                .message()
                .contains("UNIQUE constraint failed: notes.slug") =>
        {
            AppError::from(anyhow!("That slug is already in use."))
        }
        _ => AppError::from(error),
    }
}

fn render_page<T: Template>(template: &T, noindex: bool) -> AppResult<Response> {
    let html = template.render()?;
    let mut headers = HeaderMap::new();
    if noindex {
        headers.insert(
            "x-robots-tag",
            HeaderValue::from_static("noindex, nofollow, noarchive"),
        );
    }
    Ok((headers, Html(html)).into_response())
}

struct ValidatedNoteForm {
    title: String,
    slug: String,
    markdown: String,
    visibility: NoteVisibility,
    code_hash: Option<String>,
}

struct AppError {
    inner: anyhow::Error,
    redirect_to: Option<String>,
    status_code: StatusCode,
}

impl AppError {
    fn redirect(path: String) -> Self {
        Self {
            inner: anyhow!("redirect"),
            redirect_to: Some(path),
            status_code: StatusCode::SEE_OTHER,
        }
    }

    fn not_found() -> Self {
        Self {
            inner: anyhow!("not found"),
            redirect_to: None,
            status_code: StatusCode::NOT_FOUND,
        }
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(value: E) -> Self {
        Self {
            inner: value.into(),
            redirect_to: None,
            status_code: StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if let Some(path) = self.redirect_to {
            return Redirect::to(&path).into_response();
        }

        if self.status_code == StatusCode::NOT_FOUND {
            let template = NotFoundTemplate {
                page_title: "404".into(),
                body_class: "reader-page".into(),
                is_admin: false,
                noindex: true,
                show_footer: !env_flag("HIDE_FOOTER"),
            };

            return match template.render() {
                Ok(html) => {
                    let mut headers = HeaderMap::new();
                    headers.insert(
                        "x-robots-tag",
                        HeaderValue::from_static("noindex, nofollow, noarchive"),
                    );
                    (StatusCode::NOT_FOUND, headers, Html(html)).into_response()
                }
                Err(err) => {
                    error!("{:#}", err);
                    (
                        StatusCode::NOT_FOUND,
                        Html(
                            "<!doctype html><html><body style=\"font-family:system-ui;padding:2rem\"><h1>404</h1><p>Page not found.</p></body></html>",
                        ),
                    )
                        .into_response()
                }
            };
        }

        error!("{:#}", self.inner);
        (
            self.status_code,
            Html(
                "<!doctype html><html><body style=\"font-family:system-ui;padding:2rem\"><h1>Something failed</h1><p>Check the server logs for details.</p></body></html>",
            ),
        )
            .into_response()
    }
}

type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, StatusCode, header},
    };
    use serde_json::json;
    use tower::ServiceExt;

    async fn test_state() -> Arc<AppState> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        init_db(&pool).await.unwrap();

        let config = Config {
            admin_username: "admin".into(),
            admin_password: "secret".into(),
            bind_addr: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            root_redirect_url: "/dashboard".into(),
            public_base_url: Url::parse("http://localhost:8080/").unwrap(),
            passkey_rp_name: "TabNotes Tests".into(),
            hide_footer: false,
        };
        let webauthn = build_webauthn(&config).unwrap();

        Arc::new(AppState {
            pool,
            config,
            webauthn,
            pending_registrations: Mutex::new(HashMap::new()),
            pending_authentications: Mutex::new(HashMap::new()),
        })
    }

    fn test_app(state: Arc<AppState>) -> Router {
        app_router(state)
    }

    async fn response_text(response: Response) -> String {
        String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }

    async fn admin_cookie(app: &Router) -> String {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(
                        "username=admin&password=secret&next=%2Fdashboard",
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn init_db_creates_api_keys_table() {
        let state = test_state().await;
        let exists = sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'api_keys'",
        )
        .fetch_optional(&state.pool)
        .await
        .unwrap();

        assert_eq!(exists.as_deref(), Some("api_keys"));
    }

    #[tokio::test]
    async fn dashboard_renders_api_keys_section() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO notes (title, slug, visibility, markdown, updated_at) VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)",
        )
        .bind("Visible note")
        .bind("visible-note")
        .bind("public")
        .bind("Hello")
        .execute(&state.pool)
        .await
        .unwrap();
        let app = test_app(state);
        let cookie = admin_cookie(&app).await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_text(response).await;
        assert!(body.contains("API keys"));
        assert!(body.contains("🌐"));
        assert!(body.contains("Public"));
        assert!(!body.contains("Use this for authenticated requests to the notes CRUD API."));
    }

    #[tokio::test]
    async fn note_page_renders_delete_button_for_admin() {
        let state = test_state().await;
        sqlx::query(
            "INSERT INTO notes (title, slug, visibility, markdown, updated_at) VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)",
        )
        .bind("Delete me")
        .bind("delete-me")
        .bind("public")
        .bind("Hello")
        .execute(&state.pool)
        .await
        .unwrap();

        let app = test_app(state);
        let cookie = admin_cookie(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/notes/delete-me")
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_text(response).await;
        assert!(body.contains("Delete note"));
    }

    #[tokio::test]
    async fn editor_page_renders_delete_button_for_existing_note() {
        let state = test_state().await;
        let result = sqlx::query(
            "INSERT INTO notes (title, slug, visibility, markdown, updated_at) VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)",
        )
        .bind("Edit me")
        .bind("edit-me")
        .bind("admin")
        .bind("Hello")
        .execute(&state.pool)
        .await
        .unwrap();
        let note_id = result.last_insert_rowid();

        let app = test_app(state);
        let cookie = admin_cookie(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/admin/notes/{note_id}/edit"))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_text(response).await;
        assert!(body.contains("Delete note"));
        assert!(body.contains(&format!("/admin/notes/{note_id}/delete")));
    }

    #[tokio::test]
    async fn dashboard_renders_passkey_delete_button_and_delete_route_works() {
        let state = test_state().await;
        let result = sqlx::query("INSERT INTO passkeys (label, credential_json) VALUES (?1, ?2)")
            .bind("MacBook Touch ID")
            .bind("{}")
            .execute(&state.pool)
            .await
            .unwrap();
        let passkey_id = result.last_insert_rowid();

        let app = test_app(state.clone());
        let cookie = admin_cookie(&app).await;

        let dashboard_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/dashboard")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dashboard_response.status(), StatusCode::OK);
        let dashboard_body = response_text(dashboard_response).await;
        assert!(dashboard_body.contains(&format!("/admin/passkeys/{passkey_id}/delete")));

        let delete_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/admin/passkeys/{passkey_id}/delete"))
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::SEE_OTHER);

        let remaining = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM passkeys WHERE id = ?1")
            .bind(passkey_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn dashboard_styles_include_api_key_form_spacing() {
        let css = include_str!("../static/app.css");

        assert!(css.contains("#api-key-create-form + .api-key-list"));
        assert!(css.contains("margin-top: 16px;"));
    }

    #[test]
    fn mobile_styles_contain_compact_topbar_rules() {
        let css = include_str!("../static/app.css");

        assert!(css.contains(".topbar-actions > *"));
        assert!(css.contains("border-radius: var(--radius-xl);"));
    }

    #[tokio::test]
    async fn api_routes_require_api_key_and_support_note_deletion() {
        let state = test_state().await;
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS api_keys (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                label TEXT NOT NULL,
                key_prefix TEXT NOT NULL,
                key_hash TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
                last_used_at TEXT
            )
            "#,
        )
        .execute(&state.pool)
        .await
        .unwrap();

        let raw_key = "tn_testprefix12_1234567890abcdef1234567890abcdef";
        sqlx::query("INSERT INTO api_keys (label, key_prefix, key_hash) VALUES (?1, ?2, ?3)")
            .bind("Integration test")
            .bind("testprefix12")
            .bind(hash_secret(raw_key).unwrap())
            .execute(&state.pool)
            .await
            .unwrap();

        let app = test_app(state.clone());

        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/notes")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/notes")
                    .header(header::AUTHORIZATION, format!("Bearer {raw_key}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        json!({
                            "title": "API note",
                            "slug": "api-note",
                            "markdown": "Created from test",
                            "visibility": "admin"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_response.status(), StatusCode::CREATED);
        let created_body = response_text(create_response).await;
        let created_note: serde_json::Value = serde_json::from_str(&created_body).unwrap();
        let note_id = created_note["id"].as_i64().unwrap();

        let delete_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/notes/{note_id}"))
                    .header(header::AUTHORIZATION, format!("Bearer {raw_key}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

        let remaining = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notes WHERE id = ?1")
            .bind(note_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
    }
}
