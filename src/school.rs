use regex::Regex;
use reqwest::{Client, Url, cookie::Jar};
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

static COURSE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/student/course/(\d+)(?:/|$|[?#])").unwrap());
static PUNCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"/punchw/(?:course/)?(\d+)/(\d+)|/student/course/(\d+)/punchw/(\d+)").unwrap()
});
static EXPLICIT_TASK_TITLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9\u{4E00}-\u{9FFF}·（）()《》【】\-_]{2,40}签到").unwrap()
});
static ACTION_TITLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:点击(?:此处)?(?:完成)?|点此(?:去)?(?:完成)?|去|立即|马上|开始|完成).{0,4}签到$",
    )
    .unwrap()
});

#[derive(Clone, Serialize, Deserialize)]
pub struct CookieInput {
    pub name: String,
    pub value: String,
    #[serde(default = "root_path")]
    pub path: String,
    #[serde(default)]
    pub expires_at: Option<i64>, // Unix seconds, never Android milliseconds.
}
fn root_path() -> String {
    "/".into()
}

#[derive(Clone, Debug, Serialize)]
pub struct PunchTask {
    pub course_id: String,
    pub punch_id: String,
    pub title: String,
    #[serde(skip)]
    pub url: Url,
}

#[derive(Debug, thiserror::Error)]
pub enum SchoolError {
    #[error("学校凭证已失效，请重新登录并更新 Cookie")]
    CredentialsInvalid,
    #[error("学校接口暂时不可用")]
    Unavailable,
    #[error("签到结果待确认")]
    Uncertain,
    #[error("{0}")]
    Rejected(String),
}

#[derive(Debug)]
pub enum PunchState {
    AlreadyDone,
    Closed,
    Ready,
}

pub struct SchoolClient {
    client: Client,
    base: Url,
    cookies: Mutex<Vec<CookieInput>>,
}

impl SchoolClient {
    pub fn new(base: &str, cookies: &[CookieInput]) -> anyhow::Result<Self> {
        Self::new_with_proxy(base, cookies, None)
    }
    pub fn new_with_proxy(
        base: &str,
        cookies: &[CookieInput],
        proxy: Option<&str>,
    ) -> anyhow::Result<Self> {
        let base = Url::parse(base)?;
        anyhow::ensure!(
            !cookies.is_empty() && cookies.len() <= 64,
            "需要 1..64 个 Cookie"
        );
        let jar = Arc::new(Jar::default());
        for cookie in cookies {
            anyhow::ensure!(
                !cookie.name.is_empty()
                    && cookie.name.len() <= 128
                    && cookie
                        .name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c)),
                "Cookie 名称无效"
            );
            anyhow::ensure!(
                cookie.value.len() <= 8192
                    && !cookie.value.contains(['\r', '\n', ';'])
                    && cookie.value.is_ascii(),
                "Cookie 值无效"
            );
            anyhow::ensure!(
                cookie.path.starts_with('/')
                    && !cookie.path.contains(['\r', '\n', ';'])
                    && cookie.path.len() <= 256,
                "Cookie 路径无效"
            );
            if cookie
                .expires_at
                .is_some_and(|expiry| expiry <= chrono::Utc::now().timestamp())
            {
                continue;
            }
            let mut raw = format!(
                "{}={}; Path={}; HttpOnly",
                cookie.name, cookie.value, cookie.path
            );
            if base.scheme() == "https" {
                raw.push_str("; Secure");
            }
            if let Some(expiry) = cookie.expires_at {
                raw.push_str(&format!(
                    "; Max-Age={}",
                    expiry - chrono::Utc::now().timestamp()
                ));
            }
            jar.add_cookie_str(&raw, &base);
        }
        let allowed_origin = base.origin();
        let mut builder = Client::builder()
            .cookie_provider(jar.clone())
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .user_agent("Mozilla/5.0 HistAutoCheckin/0.1")
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("too many redirects")
                } else if attempt.url().origin() == allowed_origin {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }));
        if let Some(proxy) = crate::config::school_proxy(proxy)? {
            builder = builder.proxy(proxy);
        }
        let client = builder.build()?;
        Ok(Self {
            client,
            base,
            cookies: Mutex::new(cookies.to_vec()),
        })
    }

    async fn page(&self, url: Url) -> Result<String, SchoolError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| SchoolError::Unavailable)?;
        self.capture_cookies(&response);
        if response.status().is_redirection()
            || response.status() == reqwest::StatusCode::UNAUTHORIZED
        {
            return Err(SchoolError::CredentialsInvalid);
        }
        if !response.status().is_success() {
            return Err(SchoolError::Unavailable);
        }
        let text = response
            .text()
            .await
            .map_err(|_| SchoolError::Unavailable)?;
        if invalid_credentials(&text) {
            return Err(SchoolError::CredentialsInvalid);
        }
        Ok(text)
    }

    pub async fn verify(&self) -> Result<String, SchoolError> {
        let html = self
            .page(
                self.base
                    .join("/student")
                    .map_err(|_| SchoolError::Unavailable)?,
            )
            .await?;
        if !html.contains("用户中心") && !html.contains("我加入的") {
            return Err(SchoolError::Unavailable);
        }
        Ok(html)
    }

    pub async fn courses(&self) -> Result<Vec<String>, SchoolError> {
        Ok(course_ids(&self.verify().await?))
    }

    pub async fn tasks(&self, courses: &[String]) -> Result<Vec<PunchTask>, SchoolError> {
        let available = self.courses().await?;
        let selected = if courses.is_empty() {
            available.as_slice()
        } else {
            courses
        };
        let mut tasks = Vec::new();
        for course in selected {
            if !available.contains(course) {
                return Err(SchoolError::Rejected(format!("账号未加入课程 {course}")));
            }
            let html = self
                .page(
                    self.base
                        .join(&format!("/student/course/{course}/punchs"))
                        .map_err(|_| SchoolError::Unavailable)?,
                )
                .await?;
            let found = parse_tasks(&self.base, course, &html);
            if courses.is_empty() {
                // Match hist-checkin: discover the first current task automatically.
                if let Some(task) = found.into_iter().next() {
                    return Ok(vec![task]);
                }
            } else {
                tasks.extend(found);
            }
        }
        Ok(tasks)
    }

    pub async fn state(&self, task: &PunchTask) -> Result<PunchState, SchoolError> {
        let html = self.page(task.url.clone()).await?;
        if html.contains("我已签到过啦") || html.contains("签到成功") {
            Ok(PunchState::AlreadyDone)
        } else if html.contains("考勤已结束") || html.contains("签到已结束") {
            Ok(PunchState::Closed)
        } else {
            Ok(PunchState::Ready)
        }
    }

    pub async fn submit(
        &self,
        task: &PunchTask,
        latitude: f64,
        longitude: f64,
    ) -> Result<(), SchoolError> {
        // Always use the user's configured coordinates; do not silently substitute page coordinates.
        let response = self
            .client
            .post(task.url.clone())
            .header("Referer", task.url.as_str())
            .header("Origin", self.base.origin().ascii_serialization())
            .form(&[
                ("lat", latitude.to_string()),
                ("lng", longitude.to_string()),
                ("acc", "15.5".into()),
            ])
            .send()
            .await
            .map_err(|_| SchoolError::Uncertain)?;
        self.capture_cookies(&response);
        if response.status().is_redirection()
            || response.status() == reqwest::StatusCode::UNAUTHORIZED
        {
            return Err(SchoolError::CredentialsInvalid);
        }
        if !response.status().is_success() {
            return Err(SchoolError::Uncertain);
        }
        let text = response.text().await.map_err(|_| SchoolError::Uncertain)?;
        classify_submission(&text)
    }

    pub fn refreshed_cookies(&self) -> Vec<CookieInput> {
        self.cookies
            .lock()
            .unwrap()
            .iter()
            .filter(|c| {
                c.expires_at
                    .is_none_or(|t| t > chrono::Utc::now().timestamp())
            })
            .cloned()
            .collect()
    }

    fn capture_cookies(&self, response: &reqwest::Response) {
        let mut stored = self.cookies.lock().unwrap();
        for raw in response.headers().get_all(reqwest::header::SET_COOKIE) {
            let Some(cookie) = raw
                .to_str()
                .ok()
                .and_then(|s| cookie::Cookie::parse(s.to_owned()).ok())
            else {
                continue;
            };
            if let Some(domain) = cookie.domain() {
                let domain = domain.trim_start_matches('.');
                let host = self.base.host_str().unwrap_or_default();
                if host != domain && !host.ends_with(&format!(".{domain}")) {
                    continue;
                }
            }
            let path = cookie.path().map(str::to_owned).unwrap_or_else(|| {
                let path = response.url().path();
                path.rsplit_once('/')
                    .map(|(parent, _)| {
                        if parent.is_empty() {
                            "/".into()
                        } else {
                            parent.to_owned()
                        }
                    })
                    .unwrap_or_else(|| "/".into())
            });
            stored.retain(|c| c.name != cookie.name() || c.path != path);
            let expires_at = cookie
                .max_age()
                .map(|age| chrono::Utc::now().timestamp() + age.whole_seconds())
                .or_else(|| cookie.expires_datetime().map(|dt| dt.unix_timestamp()));
            if expires_at.is_none_or(|t| t > chrono::Utc::now().timestamp()) {
                stored.push(CookieInput {
                    name: cookie.name().into(),
                    value: cookie.value().into(),
                    path,
                    expires_at,
                });
            }
        }
    }
}

fn invalid_credentials(text: &str) -> bool {
    (text.contains("登录") && text.contains("学生身份验证")) || text.contains("只能在微信中完成")
}

pub fn classify_submission(text: &str) -> Result<(), SchoolError> {
    if text.contains("签到成功") || text.contains("我已签到过啦") {
        Ok(())
    } else if invalid_credentials(text) {
        Err(SchoolError::CredentialsInvalid)
    } else if text.contains("不在位置范围内") {
        Err(SchoolError::Rejected("不在位置范围内".into()))
    } else if text.contains("考勤已结束") {
        Err(SchoolError::Rejected("考勤已结束".into()))
    } else {
        Err(SchoolError::Uncertain)
    }
}

pub fn course_ids(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();
    let mut seen = HashSet::new();
    doc.select(&selector)
        .filter_map(|link| {
            COURSE_RE
                .captures(link.value().attr("href")?)
                .map(|m| m[1].to_string())
        })
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

#[derive(Serialize)]
pub struct CourseItem {
    pub id: String,
    pub name: String,
}
pub fn course_items(html: &str) -> Vec<CourseItem> {
    let document = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();
    let mut seen = HashSet::new();
    document
        .select(&selector)
        .filter_map(|link| {
            let id = COURSE_RE
                .captures(link.value().attr("href")?)
                .map(|m| m[1].to_owned())?;
            if !seen.insert(id.clone()) {
                return None;
            }
            let label = link
                .text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            let name = if label.is_empty() || ["进入", "查看", "签到"].contains(&label.as_str())
            {
                format!("课程 {id}")
            } else {
                label.chars().take(120).collect()
            };
            Some(CourseItem { id, name })
        })
        .collect()
}

fn usable_task_title(text: &str) -> Option<String> {
    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let normalized: String = title.split_whitespace().collect();
    if normalized.is_empty()
        || [
            "签到",
            "正在进行的签到",
            "正在进行",
            "进行中",
            "签到中",
            "待签到",
            "已签到",
        ]
        .contains(&normalized.as_str())
        || ACTION_TITLE_RE.is_match(&normalized)
    {
        None
    } else {
        Some(title)
    }
}

fn task_title(context: ElementRef<'_>, link: ElementRef<'_>) -> String {
    // Match hist-checkin's card metadata extraction, using the parsed DOM so
    // nested elements and distant action links remain within the same task.
    if let Some(title) = context
        .value()
        .attr("data-title")
        .and_then(usable_task_title)
    {
        return title;
    }
    for pattern in [
        "[data-title]",
        "div[class*='title'],span[class*='title'],div[class*='name'],span[class*='name'],.font-weight-bold",
        "h1,h2,h3,h4,strong,b",
    ] {
        let selector = Selector::parse(pattern).unwrap();
        for element in context.select(&selector) {
            let raw = element
                .value()
                .attr("data-title")
                .map(str::to_owned)
                .unwrap_or_else(|| element.text().collect());
            if let Some(title) = usable_task_title(&raw) {
                return title;
            }
        }
    }
    let text = context.text().collect::<Vec<_>>().join(" ");
    for found in EXPLICIT_TASK_TITLE_RE.find_iter(&text) {
        if let Some(title) = usable_task_title(found.as_str()) {
            return title;
        }
    }
    usable_task_title(&link.text().collect::<String>()).unwrap_or_else(|| "签到任务".into())
}

pub fn parse_tasks(base: &Url, course: &str, html: &str) -> Vec<PunchTask> {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("a[href]").unwrap();
    let mut seen = HashSet::new();
    doc.select(&selector)
        .filter_map(|link| {
            let raw = link.value().attr("href")?;
            if !raw.contains("punchw") {
                return None;
            }
            let url = base.join(raw).ok()?;
            if url.origin() != base.origin() {
                return None;
            }
            let punch_id = if let Some(m) = PUNCH_RE.captures(url.path()) {
                let (linked_course, punch) =
                    if let (Some(course), Some(punch)) = (m.get(1), m.get(2)) {
                        (course.as_str(), punch.as_str())
                    } else {
                        (m.get(3)?.as_str(), m.get(4)?.as_str())
                    };
                if linked_course != course {
                    return None;
                }
                punch.to_owned()
            } else {
                if url.query_pairs().any(|(key, value)| {
                    (key == "courseid" || key == "course_id") && value != course
                }) {
                    return None;
                }
                url.query_pairs()
                    .find(|(k, _)| k == "punchid" || k == "punch_id" || k == "id")
                    .map(|(_, v)| v.to_string())?
            };
            let context = ["punch-card", "card", "card-body"]
                .iter()
                .find_map(|class| {
                    link.ancestors()
                        .filter_map(ElementRef::wrap)
                        .find(|e| e.value().classes().any(|c| c == *class))
                })
                .unwrap_or(link);
            let context_text = context.text().collect::<String>();
            if ["已结束", "已关闭", "未开始", "已签到过"]
                .iter()
                .any(|s| context_text.contains(s))
                || !seen.insert(punch_id.clone())
            {
                return None;
            }
            Some(PunchTask {
                course_id: course.into(),
                punch_id,
                title: task_title(context, link),
                url,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_link_variants_and_rejects_external_origin() {
        let base = Url::parse("https://school.example").unwrap();
        let html = r#"<a href='/student/punchw/course/12/30?sid=4&amp;a=b'>签到</a><a href='/student/course/12/punchw/31'>签到</a><a href='https://evil.example/student/punchw/course/12/32'>签到</a><a href='/student/punchw/course/12/33'>已结束</a><a href='/student/punchw/course/13/34'>签到</a><div class='punch-card'>已结束<a href='/student/punchw/course/12/35'>签到</a></div>"#;
        let tasks = parse_tasks(&base, "12", html);
        assert_eq!(
            tasks
                .iter()
                .map(|t| t.punch_id.as_str())
                .collect::<Vec<_>>(),
            vec!["30", "31"]
        );
        assert_eq!(tasks[0].url.query().unwrap(), "sid=4&a=b");
        assert_eq!(
            course_ids(
                "<a href='/student/course/12/punchs'>A</a><a href='/student/course/12'>B</a>"
            ),
            vec!["12"]
        );
    }
    #[test]
    fn reads_whole_card_titles_instead_of_generic_action_links() {
        let base = Url::parse("https://school.example").unwrap();
        let html = format!(
            r#"
            <div class="card punch-card punch-card--primary">
                <div class="card-body">
                    <div class="punch-status">正在进行</div>
                    <div class="mt-2 font-weight-bold">2024级学生每晚考勤</div>
                    <div class="filler">{}</div>
                    <a href="/student/punchw/course/12/30?sid=4">点此去完成签到</a>
                </div>
            </div>
            <div class="card punch-card">
                <div class="title">高等数学签到</div>
                <a href="/student/punchw/course/12/31">点击此处完成签到</a>
            </div>
            <div class="card punch-card">
                <div class="title">大学英语签到</div>
                已结束<a href="/student/punchw/course/12/32">签到</a>
            </div>
        "#,
            "x".repeat(4500)
        );
        let tasks = parse_tasks(&base, "12", &html);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title, "2024级学生每晚考勤");
        assert_eq!(tasks[1].title, "高等数学签到");
        assert_eq!(tasks[0].punch_id, "30");
        assert_eq!(tasks[1].punch_id, "31");
    }

    #[test]
    fn extracts_title_variants_and_decodes_html_entities() {
        let base = Url::parse("https://school.example").unwrap();
        for (markup, expected) in [
            (
                "<div class='punch-card' data-title='晚间 &amp; 宿舍考勤'>LINK</div>",
                "晚间 & 宿舍考勤",
            ),
            (
                "<div class='punch-card'><span data-title='宿舍晚间考勤'></span>LINK</div>",
                "宿舍晚间考勤",
            ),
            (
                "<div class='punch-card'><h2>期末<span>考勤</span>（晚间）</h2>LINK</div>",
                "期末考勤（晚间）",
            ),
            (
                "<div class='card'><div class='card-body'><span class='task-name'>实验课考勤</span></div>LINK</div>",
                "实验课考勤",
            ),
            (
                "<div class='card-body'><div class='title'>大学英语签到</div>LINK</div>",
                "大学英语签到",
            ),
            (
                "<div class='punch-card'>高等数学签到 正在进行 LINK</div>",
                "高等数学签到",
            ),
        ] {
            let html = markup.replace(
                "LINK",
                "<a href='/student/punchw/course/12/30'>立即签到</a>",
            );
            let tasks = parse_tasks(&base, "12", &html);
            assert_eq!(tasks.len(), 1, "{html}");
            assert_eq!(tasks[0].title, expected, "{html}");
        }
    }

    #[test]
    fn generic_action_links_do_not_become_task_titles() {
        let base = Url::parse("https://school.example").unwrap();
        for action in [
            "签到",
            "立即签到",
            "点击此处完成签到",
            "点此去完成签到",
            "正在进行的签到",
        ] {
            let html = format!("<a href='/student/punchw/course/12/30'>{action}</a>");
            assert_eq!(parse_tasks(&base, "12", &html)[0].title, "签到任务");
        }
    }

    #[test]
    fn ambiguous_response_requires_verification() {
        assert!(matches!(
            classify_submission("<html>unknown</html>"),
            Err(SchoolError::Uncertain)
        ));
        assert!(classify_submission("我已签到过啦").is_ok());
        assert!(matches!(
            classify_submission("只能在微信中完成"),
            Err(SchoolError::CredentialsInvalid)
        ));
    }
}
