use axum::{
    Extension, Form, Json, Router,
    body::Body,
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, Request, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use hist_auto_ckeckin::{AppState, api, config::Config, workers};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Default)]
struct Mock {
    submissions: AtomicUsize,
    sends: AtomicUsize,
    done: AtomicBool,
    ambiguous_reply: AtomicBool,
    notify_fails: AtomicBool,
    expire_credentials: AtomicBool,
    incoming_context: RwLock<Option<String>>,
    sent_client_ids: Mutex<Vec<String>>,
    school_scan_ready: AtomicBool,
    school_requires_wechat: AtomicBool,
    school_uid: RwLock<String>,
    school_exchange_forbidden: AtomicBool,
    school_redirect_http: AtomicBool,
}

async fn mock_server(mock: Arc<Mock>) -> (String, tokio::task::JoinHandle<()>) {
    let router=Router::new()
        .route("/student",get(student))
        .route("/captcha/default",get(school_captcha))
        .route("/fields/login/student/43",post(school_identity))
        .route("/qr/weixin/student/43",get(school_qr))
        .route("/student/uidlogin",get(school_uid_login))
        .route("/student/course/12/punchs",get(||async {Html("<div class='punch-card'><a href='/student/punchw/course/12/99?sid=4'>正在进行的签到</a></div>")}))
        .route("/student/punchw/course/12/99",get(punch_state).post(submit))
        .route("/ilink/bot/get_bot_qrcode",post(qr))
        .route("/ilink/bot/get_qrcode_status",get(qr_status))
        .route("/ilink/bot/getupdates",post(updates))
        .route("/ilink/bot/sendmessage",post(send))
        .with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base, task)
}

async fn student(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
) -> (HeaderMap, Html<&'static str>) {
    let mut response = HeaderMap::new();
    if mock.expire_credentials.load(Ordering::SeqCst) {
        return (response, Html("登录 学生身份验证"));
    }
    let cookies = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        cookies.contains("remember_student_test=cookie-secret")
            || cookies.contains("remember_student_test=refreshed-secret")
    );
    response.insert(
        "set-cookie",
        "remember_student_test=refreshed-secret; Path=/; Max-Age=86400; HttpOnly"
            .parse()
            .unwrap(),
    );
    (
        response,
        Html(
            "用户中心 我加入的 <div data-student-name='张三' data-student-no='2026001'></div><a href='/student/course/12/punchs'>测试课程</a>",
        ),
    )
}

fn captcha_session(headers: &HeaderMap) {
    assert!(
        headers
            .get("cookie")
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .contains("captcha_session=school-nonce")
    );
}
async fn school_captcha() -> Response {
    let png=STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aP5sAAAAASUVORK5CYII=").unwrap();
    (
        [
            ("content-type", "image/png"),
            (
                "set-cookie",
                "captcha_session=school-nonce; Path=/; HttpOnly",
            ),
        ],
        png,
    )
        .into_response()
}
async fn school_identity(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    captcha_session(&headers);
    if form["checkcode"] != "1234" {
        return Html("验证码错误").into_response();
    }
    if mock.school_requires_wechat.load(Ordering::SeqCst) {
        return Html("已认证过并绑定了微信").into_response();
    }
    assert_eq!(form["name"], "张三");
    assert_eq!(form["no"], "2026001");
    let uid = mock.school_uid.read().await.clone();
    let uid = if uid.is_empty() {
        "3199896".to_owned()
    } else {
        uid
    };
    (
        StatusCode::FOUND,
        [(
            "location",
            format!("/student?uid={uid}&tm=1234&sign=abcdef0123456789"),
        )],
    )
        .into_response()
}
async fn school_qr(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if query.get("op").map(String::as_str) == Some("checklogin") {
        captcha_session(&headers);
        if !mock.school_scan_ready.load(Ordering::SeqCst) {
            return Json(json!({"status":false})).into_response();
        }
        let uid = mock.school_uid.read().await.clone();
        let uid = if uid.is_empty() {
            "3199896".to_owned()
        } else {
            uid
        };
        return Json(json!({"status":true,"url":format!("/student?uid={uid}&tm=1234&sign=abcdef0123456789")})).into_response();
    }
    (
        [(
            "set-cookie",
            "captcha_session=school-nonce; Path=/; HttpOnly",
        )],
        Html(r#"<script>var qrurl = "weixin://school/mock-qr";</script>"#),
    )
        .into_response()
}
async fn school_uid_login(
    State(mock): State<Arc<Mock>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    assert_eq!(query["sign"], "abcdef0123456789");
    assert!(!query["uid"].is_empty());
    if mock.school_exchange_forbidden.load(Ordering::SeqCst) {
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }
    if mock.school_redirect_http.load(Ordering::SeqCst) {
        return (
            StatusCode::FOUND,
            [
                (
                    "set-cookie",
                    "remember_student_test=cookie-secret; Path=/; Max-Age=86400; HttpOnly",
                ),
                ("location", "http://127.0.0.1/student"),
            ],
        )
            .into_response();
    }
    (
        StatusCode::FOUND,
        [
            (
                "set-cookie",
                "remember_student_test=cookie-secret; Path=/; Max-Age=86400; HttpOnly",
            ),
            ("location", "/student"),
        ],
    )
        .into_response()
}

#[sqlx::test(migrations = "./migrations")]
async fn confirmed_scan_retains_callback_and_reports_exchange_403(db: PgPool) {
    let mock = Arc::new(Mock::default());
    mock.school_scan_ready.store(true, Ordering::SeqCst);
    mock.school_exchange_forbidden.store(true, Ordering::SeqCst);
    let (base, server) = mock_server(mock.clone()).await;
    let state = state(db.clone(), &base);
    let app = app(state);
    let (_, headers, _) = web_request(
        &app,
        "POST",
        "/api/school-login/start",
        None,
        json!({"method":"wechat"}),
        true,
    )
    .await;
    let guest = cookie_from(&headers, "hist_school_login");
    let (status, _, body) = web_request(
        &app,
        "POST",
        "/api/school-login/poll",
        Some(&guest),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(body["scan_confirmed"], true);
    assert_eq!(body["code"], "school_session_exchange_failed");
    assert!(body["error"].as_str().unwrap().contains("403"));
    let saved: String = sqlx::query_scalar("SELECT callback_ciphertext FROM school_login_sessions")
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(saved.starts_with("v1:"));
    assert!(!saved.contains("abcdef0123456789"));
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 0);
    mock.school_scan_ready.store(false, Ordering::SeqCst);
    mock.school_exchange_forbidden
        .store(false, Ordering::SeqCst);
    let (status, headers, body) = web_request(
        &app,
        "POST",
        "/api/school-login/poll",
        Some(&guest),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "authenticated");
    assert!(headers.contains_key("set-cookie"));
    server.abort();
}

#[sqlx::test(migrations = "./migrations")]
async fn school_cookie_exchange_checks_fixed_origin_after_http_location(db: PgPool) {
    let mock = Arc::new(Mock::default());
    mock.school_scan_ready.store(true, Ordering::SeqCst);
    mock.school_redirect_http.store(true, Ordering::SeqCst);
    let (base, server) = mock_server(mock).await;
    let app = app(state(db, &base));
    let (_, headers, _) = web_request(
        &app,
        "POST",
        "/api/school-login/start",
        None,
        json!({"method":"wechat"}),
        true,
    )
    .await;
    let guest = cookie_from(&headers, "hist_school_login");
    let (status, _, body) = web_request(
        &app,
        "POST",
        "/api/school-login/poll",
        Some(&guest),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "authenticated");
    server.abort();
}

async fn socks_test_server(target_port: u16) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let (mut client, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                assert_eq!(client.read_u8().await.unwrap(), 5);
                let count = client.read_u8().await.unwrap();
                let mut methods = vec![0; count as usize];
                client.read_exact(&mut methods).await.unwrap();
                assert!(methods.contains(&2));
                client.write_all(&[5, 2]).await.unwrap();
                assert_eq!(client.read_u8().await.unwrap(), 1);
                let length = client.read_u8().await.unwrap();
                let mut username = vec![0; length as usize];
                client.read_exact(&mut username).await.unwrap();
                let length = client.read_u8().await.unwrap();
                let mut password = vec![0; length as usize];
                client.read_exact(&mut password).await.unwrap();
                assert_eq!(username, b"test-user");
                assert_eq!(password, b"test-password");
                client.write_all(&[1, 0]).await.unwrap();
                let mut request = [0; 4];
                client.read_exact(&mut request).await.unwrap();
                assert_eq!(&request[..3], &[5, 1, 0]);
                match request[3] {
                    1 => {
                        let mut address = [0; 4];
                        client.read_exact(&mut address).await.unwrap();
                        assert_eq!(address, [127, 0, 0, 1]);
                    }
                    3 => {
                        let length = client.read_u8().await.unwrap();
                        let mut host = vec![0; length as usize];
                        client.read_exact(&mut host).await.unwrap();
                        assert!(host == b"127.0.0.1" || host == b"localhost");
                    }
                    other => panic!("unexpected SOCKS address type {other}"),
                }
                let port = client.read_u16().await.unwrap();
                assert_eq!(port, target_port);
                let mut upstream = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                client
                    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });
    (
        format!("socks5h://test-user:test-password@127.0.0.1:{port}"),
        task,
    )
}

#[sqlx::test(migrations = "./migrations")]
async fn school_login_and_course_requests_use_configured_socks_proxy(db: PgPool) {
    let mock = Arc::new(Mock::default());
    mock.school_scan_ready.store(true, Ordering::SeqCst);
    let (base, server) = mock_server(mock).await;
    let target = reqwest::Url::parse(&base).unwrap().port().unwrap();
    let (proxy, relay) = socks_test_server(target).await;
    let mut config = (*state(db.clone(), &base).config).clone();
    config.school_proxy_url = Some(proxy);
    let app = app(AppState::new(db, config).unwrap());
    let (_, headers, _) = web_request(
        &app,
        "POST",
        "/api/school-login/start",
        None,
        json!({"method":"wechat"}),
        true,
    )
    .await;
    let guest = cookie_from(&headers, "hist_school_login");
    let (status, headers, body) = web_request(
        &app,
        "POST",
        "/api/school-login/poll",
        Some(&guest),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let account = body["school_account_id"].as_str().unwrap();
    let cookie = cookie_from(&headers, "hist_session");
    let (status, _, body) = web_request(
        &app,
        "GET",
        &format!("/api/school-accounts/{account}/courses"),
        Some(&cookie),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["course_ids"], json!(["12"]));
    relay.abort();
    server.abort();
}

async fn punch_state(State(mock): State<Arc<Mock>>) -> Html<&'static str> {
    if mock.done.load(Ordering::SeqCst) {
        Html("我已签到过啦")
    } else {
        Html("正在进行的签到")
    }
}

async fn submit(
    State(mock): State<Arc<Mock>>,
    Form(form): Form<HashMap<String, String>>,
) -> Html<&'static str> {
    assert_eq!(form["lat"], "35.281256");
    assert_eq!(form["lng"], "113.935975");
    assert_eq!(form["acc"], "15.5");
    mock.submissions.fetch_add(1, Ordering::SeqCst);
    mock.done.store(true, Ordering::SeqCst);
    if mock.ambiguous_reply.load(Ordering::SeqCst) {
        Html("未识别的响应")
    } else {
        Html("签到成功")
    }
}

fn check_post_headers(headers: &HeaderMap, authenticated: bool) {
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(headers["AuthorizationType"], "ilink_bot_token");
    assert_eq!(headers["iLink-App-Id"], "bot");
    assert_eq!(headers["iLink-App-ClientVersion"], "132105");
    assert!(
        STANDARD
            .decode(headers["X-WECHAT-UIN"].to_str().unwrap())
            .is_ok()
    );
    if authenticated {
        assert_eq!(headers["authorization"], "Bearer bot-secret");
    } else {
        assert!(!headers.contains_key("authorization"));
    }
}

async fn qr(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    check_post_headers(&headers, false);
    assert!(body["local_token_list"].is_array());
    assert!(body.get("base_info").is_none());
    Json(json!({"qrcode":"qrcode-secret","qrcode_img_content":"https://example.test/qr-image"}))
}

async fn qr_status(
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Json<Value> {
    assert!(!headers.contains_key("authorizationtype"));
    assert_eq!(headers["iLink-App-Id"], "bot");
    assert_eq!(query["qrcode"], "qrcode-secret");
    if query.get("verify_code").map(String::as_str) != Some("1234") {
        return Json(json!({"status":"need_verifycode"}));
    }
    Json(
        json!({"status":"confirmed","bot_token":"bot-secret","ilink_bot_id":"bot-id","ilink_user_id":"recipient"}),
    )
}

async fn updates(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    check_post_headers(&headers, true);
    assert_eq!(body["base_info"]["channel_version"], "2.4.9");
    let mut messages = vec![
        json!({"from_user_id":"someone-else","message_type":1,"context_token":"foreign-context"}),
    ];
    if let Some(context) = mock.incoming_context.read().await.clone() {
        messages.push(json!({"from_user_id":"recipient","message_type":1,"context_token":context}));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    Json(
        json!({"ret":0,"msgs":messages,"get_updates_buf":"cursor-secret","longpolling_timeout_ms":35000}),
    )
}

async fn send(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    check_post_headers(&headers, true);
    let msg = &body["msg"];
    assert_eq!(msg["to_user_id"], "recipient");
    assert_eq!(msg["context_token"], "context-secret");
    assert_eq!(msg["message_type"], 2);
    assert_eq!(msg["message_state"], 2);
    assert!(
        msg["item_list"][0]["text_item"]["text"]
            .as_str()
            .unwrap()
            .contains("Hist-Auto-Checkin")
    );
    mock.sent_client_ids
        .lock()
        .await
        .push(msg["client_id"].as_str().unwrap().into());
    mock.sends.fetch_add(1, Ordering::SeqCst);
    Json(if mock.notify_fails.load(Ordering::SeqCst) {
        json!({"ret":-1,"errmsg":"mock failure"})
    } else {
        json!({"ret":0})
    })
}

fn state(db: PgPool, base: &str) -> AppState {
    AppState::new(
        db,
        Config {
            database_url: String::new(),
            credential_key: STANDARD.encode([7; 32]),
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            allow_registration: true,
            auth_rate_limit: 120,
            trusted_proxies: vec![],
            worker_concurrency: 2,
            poll_concurrency: 4,
            max_connections: 16,
            school_base_url: base.into(),
            school_login_base_url: base.into(),
            school_proxy_url: None,
            cookie_secure: false,
            weixin_base_url: base.into(),
            weixin_version: "2.4.9".into(),
        },
    )
    .unwrap()
}

fn app(state: AppState) -> Router {
    api::router(state).layer(Extension(ConnectInfo(
        "127.0.0.1:45678".parse::<SocketAddr>().unwrap(),
    )))
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&bytes)}))
    };
    (status, value)
}

async fn register(app: &Router, name: &str) -> (Uuid, String) {
    let (status, body) = request(
        app,
        "POST",
        "/api/auth/register",
        None,
        json!({"username":name,"password":"test-password-123"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    (
        body["user_id"].as_str().unwrap().parse().unwrap(),
        body["token"].as_str().unwrap().into(),
    )
}

async fn school_account(app: &Router, token: &str) -> Uuid {
    let (status,body)=request(app,"POST","/api/school-accounts",Some(token),json!({"label":"测试账号","student_no":"2026001","cookies":[{"name":"remember_student_test","value":"cookie-secret"}]})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().parse().unwrap()
}

fn schedule_input(account: Uuid) -> Value {
    json!({"school_account_id":account,"course_ids":["12"],"local_time":"08:30","weekdays":[1,2,3,4,5,6,7],"latitude":35.281256,"longitude":113.935975,"location_name":"1号宿舍楼","retry_window_minutes":10})
}

async fn schedule(app: &Router, token: &str, account: Uuid) -> Uuid {
    let (status, body) = request(
        app,
        "POST",
        "/api/schedules",
        Some(token),
        schedule_input(account),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().parse().unwrap()
}

async fn run(app: &Router, token: &str, schedule: Uuid) -> Uuid {
    let (status, body) = request(
        app,
        "POST",
        &format!("/api/schedules/{schedule}/run"),
        Some(token),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    body["job_id"].as_str().unwrap().parse().unwrap()
}

async fn bind_weixin(app: &Router, token: &str) {
    let (status, body) = request(app, "POST", "/api/weixin/login", Some(token), json!({})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.get("qrcode").is_none());
    let id = body["session_id"].as_str().unwrap();
    let (_, body) = request(
        app,
        "POST",
        &format!("/api/weixin/login/{id}/poll"),
        Some(token),
        json!({}),
    )
    .await;
    assert_eq!(body["status"], "need_verifycode");
    let (status, body) = request(
        app,
        "POST",
        &format!("/api/weixin/login/{id}/poll"),
        Some(token),
        json!({"verify_code":"1234"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "confirmed");
    assert!(body.get("bot_token").is_none());
}

async fn wait_status(db: &PgPool, table: &str, id: Uuid, desired: &str) {
    assert!(["checkin_jobs", "notification_outbox"].contains(&table));
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let query = format!(
                "SELECT status FROM {table} WHERE {}=$1",
                if table == "checkin_jobs" {
                    "id"
                } else {
                    "job_id"
                }
            );
            let status: Option<String> = sqlx::query_scalar(&query)
                .bind(id)
                .fetch_optional(db)
                .await
                .unwrap();
            if status.as_deref() == Some(desired) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {table} to become {desired}"));
}

#[sqlx::test(migrations = "./migrations")]
async fn registration_tenant_isolation_and_encrypted_credentials(db: PgPool) {
    let mock = Arc::new(Mock::default());
    let (base, server) = mock_server(mock).await;
    let state = state(db.clone(), &base);
    let app = app(state.clone());
    let (user, token) = register(&app, "alice").await;
    let (_, other) = register(&app, "bob").await;
    let account = school_account(&app, &token).await;
    let (status, list) =
        request(&app, "GET", "/api/school-accounts", Some(&token), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!list.to_string().contains("secret"));
    let (status, list) =
        request(&app, "GET", "/api/school-accounts", Some(&other), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list, json!([]));
    let (status, _) = request(
        &app,
        "GET",
        &format!("/api/school-accounts/{account}/courses"),
        Some(&other),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(
        &app,
        "POST",
        "/api/schedules",
        Some(&other),
        schedule_input(account),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let raw: String =
        sqlx::query_scalar("SELECT cookies_ciphertext FROM school_accounts WHERE id=$1")
            .bind(account)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(raw.starts_with("v1:"));
    assert!(!raw.contains("secret"));
    let plaintext = state
        .secrets
        .decrypt(&raw, &format!("school:{user}:{account}"))
        .unwrap();
    assert!(plaintext.contains("refreshed-secret"));
    assert!(
        state
            .secrets
            .decrypt(&raw, &format!("school:{}:{account}", Uuid::new_v4()))
            .is_err()
    );
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id=$1")
        .bind(user)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(hash.starts_with("$argon2id$"));
    let (status, _) = request(
        &app,
        "POST",
        "/api/auth/login",
        None,
        json!({"username":"alice","password":"wrong-password"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = request(&app, "POST", "/api/auth/logout", Some(&token), json!({})).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = request(&app, "GET", "/api/me", Some(&token), json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    server.abort();
}

#[sqlx::test(migrations = "./migrations")]
async fn queue_claims_serialize_account_and_recover_expired_leases(db: PgPool) {
    let (base, server) = mock_server(Arc::new(Mock::default())).await;
    let state = state(db.clone(), &base);
    let app = app(state.clone());
    let (_, token) = register(&app, "alice").await;
    let account = school_account(&app, &token).await;
    let schedule = schedule(&app, &token, account).await;
    run(&app, &token, schedule).await;
    run(&app, &token, schedule).await;
    let (first, second) = tokio::join!(workers::claim_job(&state), workers::claim_job(&state));
    let claims = [first.unwrap(), second.unwrap()];
    assert_eq!(claims.iter().filter(|c| c.is_some()).count(), 1);
    let (job, _, _) = claims.into_iter().flatten().next().unwrap();
    let mut changed = schedule_input(account);
    changed["enabled"] = json!(false);
    let (status, _) = request(
        &app,
        "PUT",
        &format!("/api/schedules/{schedule}"),
        Some(&token),
        changed,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    sqlx::query("UPDATE checkin_jobs SET lease_until=now()-interval '1 second' WHERE id=$1")
        .bind(job.id)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("UPDATE school_accounts SET execution_until=now()-interval '1 second' WHERE id=$1")
        .bind(account)
        .execute(&db)
        .await
        .unwrap();
    workers::schedule_tick(&state).await.unwrap();
    let (recovered, _, _) = workers::claim_job(&state).await.unwrap().unwrap();
    assert_ne!(job.lease_token, recovered.lease_token);
    let stale_update =
        sqlx::query("UPDATE checkin_jobs SET status='succeeded' WHERE id=$1 AND lease_token=$2")
            .bind(job.id)
            .bind(job.lease_token)
            .execute(&db)
            .await
            .unwrap();
    assert_eq!(stale_update.rows_affected(), 0);
    server.abort();
}

#[sqlx::test(migrations = "./migrations")]
async fn scheduled_occurrence_and_deadline_notification_are_atomic(db: PgPool) {
    let (base, server) = mock_server(Arc::new(Mock::default())).await;
    let state = state(db.clone(), &base);
    let app = app(state.clone());
    let (_, token) = register(&app, "alice").await;
    let account = school_account(&app, &token).await;
    let schedule = schedule(&app, &token, account).await;
    sqlx::query("UPDATE checkin_schedules SET next_run_at=now()-interval '1 hour' WHERE id=$1")
        .bind(schedule)
        .execute(&db)
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        workers::schedule_tick(&state),
        workers::schedule_tick(&state)
    );
    a.unwrap();
    b.unwrap();
    let counts: (i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM checkin_jobs WHERE schedule_id=$1),(SELECT count(*) FROM notification_outbox)").bind(schedule).fetch_one(&db).await.unwrap();
    assert_eq!(counts, (1, 1));
    let status: String = sqlx::query_scalar("SELECT status FROM checkin_jobs WHERE schedule_id=$1")
        .bind(schedule)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(status, "failed");
    server.abort();
}

#[sqlx::test(migrations = "./migrations")]
async fn full_checkin_weixin_flow_retries_notification_without_repunch(db: PgPool) {
    let mock = Arc::new(Mock::default());
    mock.notify_fails.store(true, Ordering::SeqCst);
    let (base, server) = mock_server(mock.clone()).await;
    let state = state(db.clone(), &base);
    let app = app(state.clone());
    let (_, token) = register(&app, "alice").await;
    let account = school_account(&app, &token).await;
    let schedule = schedule(&app, &token, account).await;
    bind_weixin(&app, &token).await;
    let cancel = CancellationToken::new();
    let tasks = workers::start(state, cancel.clone());
    let job = run(&app, &token, schedule).await;
    wait_status(&db, "checkin_jobs", job, "succeeded").await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        mock.sends.load(Ordering::SeqCst),
        0,
        "foreign sender must not provide context"
    );
    let (_, binding) = request(&app, "GET", "/api/weixin/binding", Some(&token), json!({})).await;
    assert_eq!(binding["has_context"], false);
    *mock.incoming_context.write().await = Some("context-secret".into());
    tokio::time::timeout(Duration::from_secs(8), async {
        while mock.sends.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    wait_status(&db, "notification_outbox", job, "pending").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        mock.sends.load(Ordering::SeqCst),
        1,
        "message polling must not reset notification error backoff"
    );
    mock.notify_fails.store(false, Ordering::SeqCst);
    sqlx::query("UPDATE notification_outbox SET available_at=now() WHERE job_id=$1")
        .bind(job)
        .execute(&db)
        .await
        .unwrap();
    wait_status(&db, "notification_outbox", job, "sent").await;
    assert_eq!(mock.submissions.load(Ordering::SeqCst), 1);
    assert!(mock.sends.load(Ordering::SeqCst) >= 2);
    let ids = mock.sent_client_ids.lock().await;
    assert!(
        ids.iter().all(|id| id == &ids[0]),
        "notification retries must retain client_id"
    );
    let (_, jobs) = request(&app, "GET", "/api/jobs", Some(&token), json!({})).await;
    assert_eq!(jobs["jobs"][0]["notification_status"], "sent");
    assert_eq!(jobs["jobs"][0]["records"][0]["latitude"], 35.281256);
    cancel.cancel();
    for task in tasks {
        task.await.unwrap();
    }
    server.abort();
}

#[sqlx::test(migrations = "./migrations")]
async fn ambiguous_submission_is_verified_before_any_retry(db: PgPool) {
    let mock = Arc::new(Mock::default());
    mock.ambiguous_reply.store(true, Ordering::SeqCst);
    let (base, server) = mock_server(mock.clone()).await;
    let state = state(db.clone(), &base);
    let app = app(state.clone());
    let (_, token) = register(&app, "alice").await;
    let account = school_account(&app, &token).await;
    let schedule = schedule(&app, &token, account).await;
    let job = run(&app, &token, schedule).await;
    let cancel = CancellationToken::new();
    let tasks = workers::start(state, cancel.clone());
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let status: Option<String> =
                sqlx::query_scalar("SELECT status FROM checkin_records WHERE job_id=$1")
                    .bind(job)
                    .fetch_optional(&db)
                    .await
                    .unwrap();
            if status.as_deref() == Some("uncertain") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    wait_status(&db, "checkin_jobs", job, "pending").await;
    sqlx::query("UPDATE checkin_jobs SET available_at=now() WHERE id=$1")
        .bind(job)
        .execute(&db)
        .await
        .unwrap();
    wait_status(&db, "checkin_jobs", job, "succeeded").await;
    assert_eq!(mock.submissions.load(Ordering::SeqCst), 1);
    cancel.cancel();
    for task in tasks {
        task.await.unwrap();
    }
    server.abort();
}

#[sqlx::test(migrations = "./migrations")]
async fn invalid_school_credentials_pause_rules_and_enqueue_result(db: PgPool) {
    let mock = Arc::new(Mock::default());
    let (base, server) = mock_server(mock.clone()).await;
    let state = state(db.clone(), &base);
    let app = app(state.clone());
    let (_, token) = register(&app, "alice").await;
    let account = school_account(&app, &token).await;
    let schedule = schedule(&app, &token, account).await;
    mock.expire_credentials.store(true, Ordering::SeqCst);
    let job = run(&app, &token, schedule).await;
    let cancel = CancellationToken::new();
    let tasks = workers::start(state, cancel.clone());
    wait_status(&db, "checkin_jobs", job, "failed").await;
    let row: (String,bool,i64)=sqlx::query_as("SELECT a.credential_status,s.enabled,(SELECT count(*) FROM notification_outbox WHERE job_id=$1) FROM school_accounts a JOIN checkin_schedules s ON s.school_account_id=a.id WHERE s.id=$2").bind(job).bind(schedule).fetch_one(&db).await.unwrap();
    assert_eq!(row, ("invalid".into(), false, 1));
    assert_eq!(mock.submissions.load(Ordering::SeqCst), 0);
    cancel.cancel();
    for task in tasks {
        task.await.unwrap();
    }
    server.abort();
}

async fn web_request(
    app: &Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    body: Value,
    marker: bool,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if marker {
        request = request.header("x-hist-request", "1");
    }
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body)
}
fn cookie_from(headers: &HeaderMap, name: &str) -> String {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|h| h.to_str().ok())
        .find(|cookie| cookie.starts_with(&format!("{name}=")))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

#[sqlx::test(migrations = "./migrations")]
async fn web_identity_login_saves_credentials_without_service_password(db: PgPool) {
    let (base, server) = mock_server(Arc::new(Mock::default())).await;
    let state = state(db.clone(), &base);
    let app = app(state.clone());
    let (status, headers, body) = web_request(
        &app,
        "POST",
        "/api/school-login/start",
        None,
        json!({"method":"identity"}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["captcha_data_url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    let guest = cookie_from(&headers, "hist_school_login");
    let (status, _, _) = web_request(
        &app,
        "POST",
        "/api/school-login/identity",
        Some(&guest),
        json!({"name":"张三","student_no":"2026001","captcha_code":"wrong"}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, headers, body) = web_request(
        &app,
        "POST",
        "/api/school-login/identity",
        Some(&guest),
        json!({"name":"张三","student_no":"2026001","captcha_code":"1234"}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "authenticated");
    assert!(body.get("token").is_none());
    let set = headers
        .get_all("set-cookie")
        .iter()
        .find(|h| h.to_str().unwrap().starts_with("hist_session="))
        .unwrap()
        .to_str()
        .unwrap();
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax"));
    let session = cookie_from(&headers, "hist_session");
    let (status, _, me) =
        web_request(&app, "GET", "/api/me", Some(&session), json!({}), true).await;
    assert_eq!(status, StatusCode::OK);
    let user: Uuid = me["id"].as_str().unwrap().parse().unwrap();
    let account: Uuid = body["school_account_id"].as_str().unwrap().parse().unwrap();
    let (hash,ciphertext): (Option<String>,String)=sqlx::query_as("SELECT u.password_hash,a.cookies_ciphertext FROM users u JOIN school_accounts a ON a.user_id=u.id WHERE u.id=$1").bind(user).fetch_one(&db).await.unwrap();
    assert!(hash.is_none());
    assert!(!ciphertext.contains("secret"));
    assert!(
        state
            .secrets
            .decrypt(&ciphertext, &format!("school:{user}:{account}"))
            .unwrap()
            .contains("refreshed-secret")
    );
    let (status, _, _) = web_request(
        &app,
        "POST",
        "/api/schedules",
        Some(&session),
        schedule_input(account),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = web_request(
        &app,
        "POST",
        "/api/schedules",
        Some(&session),
        schedule_input(account),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, _) = web_request(
        &app,
        "POST",
        "/api/auth/logout",
        Some(&session),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = web_request(&app, "GET", "/api/me", Some(&session), json!({}), true).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    server.abort();
}

#[sqlx::test(migrations = "./migrations")]
async fn web_school_qr_relogin_keeps_tasks_and_isolates_different_identities(db: PgPool) {
    let mock = Arc::new(Mock::default());
    mock.school_scan_ready.store(true, Ordering::SeqCst);
    let (base, server) = mock_server(mock.clone()).await;
    let state = state(db.clone(), &base);
    let app = app(state);
    let (_, headers, _) = web_request(
        &app,
        "POST",
        "/api/school-login/start",
        None,
        json!({"method":"wechat"}),
        true,
    )
    .await;
    let guest = cookie_from(&headers, "hist_school_login");
    let (status, headers, body) = web_request(
        &app,
        "POST",
        "/api/school-login/poll",
        Some(&guest),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let account: Uuid = body["school_account_id"].as_str().unwrap().parse().unwrap();
    let session = cookie_from(&headers, "hist_session");
    let (status, _, _) = web_request(
        &app,
        "POST",
        "/api/schedules",
        Some(&session),
        schedule_input(account),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, headers, _) = web_request(
        &app,
        "POST",
        "/api/school-login/start",
        Some(&session),
        json!({"method":"wechat"}),
        true,
    )
    .await;
    let guest = cookie_from(&headers, "hist_school_login");
    let (status, headers, body) = web_request(
        &app,
        "POST",
        "/api/school-login/poll",
        Some(&guest),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["school_account_id"], account.to_string());
    let session = cookie_from(&headers, "hist_session");
    let (_, _, schedules) = web_request(
        &app,
        "GET",
        "/api/schedules",
        Some(&session),
        json!({}),
        true,
    )
    .await;
    assert_eq!(schedules.as_array().unwrap().len(), 1);
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(users, 1);
    *mock.school_uid.write().await = "9999999".into();
    let (_, headers, _) = web_request(
        &app,
        "POST",
        "/api/school-login/start",
        None,
        json!({"method":"wechat"}),
        true,
    )
    .await;
    let guest = cookie_from(&headers, "hist_school_login");
    let (_, headers, body) = web_request(
        &app,
        "POST",
        "/api/school-login/poll",
        Some(&guest),
        json!({}),
        true,
    )
    .await;
    assert_ne!(body["school_account_id"], account.to_string());
    let other = cookie_from(&headers, "hist_session");
    let (_, _, schedules) =
        web_request(&app, "GET", "/api/schedules", Some(&other), json!({}), true).await;
    assert_eq!(schedules, json!([]));
    server.abort();
}

#[sqlx::test(migrations = "./migrations")]
async fn school_identity_requires_wechat_and_login_capability_cannot_be_forged(db: PgPool) {
    let mock = Arc::new(Mock::default());
    mock.school_requires_wechat.store(true, Ordering::SeqCst);
    let (base, server) = mock_server(mock.clone()).await;
    let app = app(state(db.clone(), &base));
    let (status, _, _) = web_request(
        &app,
        "POST",
        "/api/school-login/start",
        None,
        json!({"method":"wechat"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, headers, _) = web_request(
        &app,
        "POST",
        "/api/school-login/start",
        None,
        json!({"method":"identity"}),
        true,
    )
    .await;
    let guest = cookie_from(&headers, "hist_school_login");
    let (status, _, body) = web_request(
        &app,
        "POST",
        "/api/school-login/identity",
        Some(&guest),
        json!({"name":"张三","student_no":"2026001","captcha_code":"1234"}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "need_wechat");
    let forged = format!("{}.forged", guest.split('.').next().unwrap());
    let (status, _, _) = web_request(
        &app,
        "POST",
        "/api/school-login/poll",
        Some(&forged),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, body) = web_request(
        &app,
        "POST",
        "/api/school-login/poll",
        Some(&guest),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "waiting");
    mock.school_scan_ready.store(true, Ordering::SeqCst);
    let (status, _, body) = web_request(
        &app,
        "POST",
        "/api/school-login/poll",
        Some(&guest),
        json!({}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "authenticated");
    server.abort();
}
