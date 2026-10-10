use regex::Regex;
use reqwest::{Client, Url, cookie::Jar};
use scraper::{Html, Selector};
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
        let available: HashSet<_> = self.courses().await?.into_iter().collect();
        let mut tasks = Vec::new();
        for course in courses {
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
            tasks.extend(parse_tasks(&self.base, course, &html));
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
            let title = link.text().collect::<String>().trim().to_owned();
            let context = link
                .ancestors()
                .filter_map(scraper::ElementRef::wrap)
                .find(|e| e.value().classes().any(|c| c == "punch-card"))
                .map(|e| e.text().collect::<String>())
                .unwrap_or_else(|| title.clone());
            if ["已结束", "已关闭", "未开始", "已签到过"]
                .iter()
                .any(|s| context.contains(s))
                || !seen.insert(punch_id.clone())
            {
                return None;
            }
            Some(PunchTask {
                course_id: course.into(),
                punch_id,
                title,
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
