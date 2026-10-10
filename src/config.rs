use std::{env, net::SocketAddr};

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub credential_key: String,
    pub bind_addr: SocketAddr,
    pub allow_registration: bool,
    pub auth_rate_limit: i32,
    pub trusted_proxies: Vec<ipnet::IpNet>,
    pub worker_concurrency: usize,
    pub poll_concurrency: usize,
    pub max_connections: u32,
    pub school_base_url: String,
    pub school_login_base_url: String,
    pub school_proxy_url: Option<String>,
    pub cookie_secure: bool,
    pub weixin_base_url: String,
    pub weixin_version: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let config = Self {
            database_url: env::var("DATABASE_URL")?,
            credential_key: env::var("CREDENTIAL_KEY_BASE64")?,
            bind_addr: value("BIND_ADDR", "0.0.0.0:38080").parse()?,
            allow_registration: value("ALLOW_REGISTRATION", "true").parse()?,
            auth_rate_limit: value("AUTH_RATE_LIMIT_PER_IP", "120").parse()?,
            trusted_proxies: value("TRUSTED_PROXY_CIDRS", "")
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().parse())
                .collect::<std::result::Result<_, _>>()?,
            worker_concurrency: value("WORKER_CONCURRENCY", "4").parse()?,
            poll_concurrency: value("WEIXIN_POLL_CONCURRENCY", "32").parse()?,
            max_connections: value("DATABASE_MAX_CONNECTIONS", "20").parse()?,
            school_base_url: required_value("SCHOOL_BASE_URL")?,
            school_login_base_url: required_value("SCHOOL_LOGIN_BASE_URL")?,
            school_proxy_url: env::var("SCHOOL_PROXY_URL")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            cookie_secure: value("COOKIE_SECURE", "false").parse()?,
            weixin_base_url: value("WEIXIN_BASE_URL", "https://ilinkai.weixin.qq.com"),
            weixin_version: value("WEIXIN_CHANNEL_VERSION", "2.4.9"),
        };
        anyhow::ensure!(
            (1..=64).contains(&config.worker_concurrency),
            "WORKER_CONCURRENCY must be 1..64"
        );
        anyhow::ensure!(
            (1..=512).contains(&config.poll_concurrency),
            "WEIXIN_POLL_CONCURRENCY must be 1..512"
        );
        anyhow::ensure!(
            config.max_connections >= 4,
            "DATABASE_MAX_CONNECTIONS must be >=4"
        );
        anyhow::ensure!(
            config.auth_rate_limit > 0,
            "AUTH_RATE_LIMIT_PER_IP must be positive"
        );
        for base in [
            &config.school_base_url,
            &config.school_login_base_url,
            &config.weixin_base_url,
        ] {
            let url = reqwest::Url::parse(base)?;
            anyhow::ensure!(
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "upstream URLs must use HTTPS without credentials/query/fragment"
            );
        }
        crate::weixin::version_number(&config.weixin_version)?;
        school_proxy(config.school_proxy_url.as_deref())?;
        Ok(config)
    }
}

pub fn school_proxy(value: Option<&str>) -> anyhow::Result<Option<reqwest::Proxy>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed =
        reqwest::Url::parse(value).map_err(|_| anyhow::anyhow!("SCHOOL_PROXY_URL 格式无效"))?;
    anyhow::ensure!(
        ["socks5", "socks5h", "http", "https"].contains(&parsed.scheme())
            && parsed.host_str().is_some(),
        "SCHOOL_PROXY_URL 协议或地址无效"
    );
    reqwest::Proxy::all(value)
        .map(Some)
        .map_err(|_| anyhow::anyhow!("SCHOOL_PROXY_URL 无法用于代理配置"))
}

fn value(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn required_value(key: &str) -> anyhow::Result<String> {
    let value = env::var(key).map_err(|_| anyhow::anyhow!("{key} must be set"))?;
    anyhow::ensure!(!value.trim().is_empty(), "{key} must not be empty");
    Ok(value)
}
