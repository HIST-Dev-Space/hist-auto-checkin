//! iLink protocol adapted from Tencent/openclaw-weixin (MIT). See THIRD_PARTY_NOTICES.md.
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::Rng;
use reqwest::{Client, Url, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone)]
pub struct WeixinClient {
    client: Client,
    base: Url,
    version: String,
    client_version: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum WeixinError {
    #[error("微信接口暂时不可用")]
    Unavailable,
    #[error("微信长轮询超时")]
    Timeout,
    #[error("微信接口返回错误代码 {0}")]
    Api(i64),
    #[error("微信尚未收到用户消息，请先向 Bot 发送消息")]
    MissingContext,
    #[error("微信返回了无效的地址或响应")]
    InvalidResponse,
}

#[derive(Deserialize, Serialize)]
pub struct QrCode {
    pub qrcode: String,
    pub qrcode_img_content: String,
}

#[derive(Deserialize)]
pub struct QrStatus {
    pub status: String,
    pub bot_token: Option<String>,
    pub ilink_bot_id: Option<String>,
    pub ilink_user_id: Option<String>,
    pub baseurl: Option<String>,
    pub redirect_host: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct Updates {
    #[serde(default)]
    pub msgs: Vec<IncomingMessage>,
    pub get_updates_buf: Option<String>,
    pub longpolling_timeout_ms: Option<u64>,
}

#[derive(Deserialize)]
pub struct IncomingMessage {
    pub from_user_id: Option<String>,
    pub context_token: Option<String>,
    pub message_type: Option<i32>,
}

pub fn version_number(version: &str) -> anyhow::Result<u32> {
    let parts: Vec<u8> = version
        .split('.')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    anyhow::ensure!(
        parts.len() == 3,
        "WEIXIN_CHANNEL_VERSION must be major.minor.patch with 0..255 components"
    );
    Ok((u32::from(parts[0]) << 16) | (u32::from(parts[1]) << 8) | u32::from(parts[2]))
}

impl WeixinClient {
    pub fn new(base: &str, version: &str) -> anyhow::Result<Self> {
        Ok(Self {
            client: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(40))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base: Url::parse(base)?,
            version: version.into(),
            client_version: version_number(version)?,
        })
    }

    pub fn default_base(&self) -> String {
        self.base.as_str().trim_end_matches('/').to_owned()
    }

    pub fn validated_base(&self, value: &str) -> Result<Url, WeixinError> {
        let url = Url::parse(value).map_err(|_| WeixinError::InvalidResponse)?;
        let host = url.host_str().ok_or(WeixinError::InvalidResponse)?;
        let trusted = url.origin() == self.base.origin()
            || (url.scheme() == "https"
                && host.ends_with(".weixin.qq.com")
                && url.port_or_known_default() == Some(443));
        if !trusted
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || (url.path() != "/" && !url.path().is_empty())
        {
            return Err(WeixinError::InvalidResponse);
        }
        Ok(url)
    }

    fn headers(&self, authenticated_post: bool, token: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("iLink-App-Id", "bot".parse().unwrap());
        headers.insert(
            "iLink-App-ClientVersion",
            self.client_version.to_string().parse().unwrap(),
        );
        if authenticated_post {
            headers.insert("AuthorizationType", "ilink_bot_token".parse().unwrap());
            let uin = rand::thread_rng().r#gen::<u32>().to_string();
            headers.insert("X-WECHAT-UIN", STANDARD.encode(uin).parse().unwrap());
        }
        if let Some(token) = token
            && let Ok(mut value) = format!("Bearer {token}").parse::<reqwest::header::HeaderValue>()
        {
            value.set_sensitive(true);
            headers.insert("Authorization", value);
        }
        headers
    }

    fn base_info(&self) -> Value {
        json!({"channel_version":self.version,"bot_agent":"HistAutoCheckin/0.1.0"})
    }

    async fn post(
        &self,
        base: &str,
        path: &str,
        token: Option<&str>,
        body: Value,
        timeout: Duration,
    ) -> Result<Value, WeixinError> {
        let url = self
            .validated_base(base)?
            .join(path)
            .map_err(|_| WeixinError::InvalidResponse)?;
        let response = self
            .client
            .post(url)
            .headers(self.headers(true, token))
            .timeout(timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    WeixinError::Timeout
                } else {
                    WeixinError::Unavailable
                }
            })?;
        if !response.status().is_success() {
            return Err(WeixinError::Unavailable);
        }
        let result: Value = response
            .json()
            .await
            .map_err(|_| WeixinError::InvalidResponse)?;
        check_response(&result)?;
        Ok(result)
    }

    pub async fn qr_code(&self, local_tokens: Vec<String>) -> Result<QrCode, WeixinError> {
        let body = self
            .post(
                &self.default_base(),
                "ilink/bot/get_bot_qrcode?bot_type=3",
                None,
                json!({"local_token_list":local_tokens}),
                Duration::from_secs(15),
            )
            .await?;
        serde_json::from_value(body).map_err(|_| WeixinError::InvalidResponse)
    }

    pub async fn qr_status(
        &self,
        base: &str,
        qrcode: &str,
        verify_code: Option<&str>,
    ) -> Result<QrStatus, WeixinError> {
        let url = self
            .validated_base(base)?
            .join("ilink/bot/get_qrcode_status")
            .map_err(|_| WeixinError::InvalidResponse)?;
        let mut query = vec![("qrcode", qrcode)];
        if let Some(code) = verify_code {
            query.push(("verify_code", code));
        }
        let response = match self
            .client
            .get(url)
            .headers(self.headers(false, None))
            .query(&query)
            .timeout(Duration::from_secs(35))
            .send()
            .await
        {
            Err(e) if e.is_timeout() => {
                return Ok(QrStatus {
                    status: "wait".into(),
                    bot_token: None,
                    ilink_bot_id: None,
                    ilink_user_id: None,
                    baseurl: None,
                    redirect_host: None,
                });
            }
            Err(_) => return Err(WeixinError::Unavailable),
            Ok(response) => response,
        };
        if !response.status().is_success() {
            return Err(WeixinError::Unavailable);
        }
        response
            .json()
            .await
            .map_err(|_| WeixinError::InvalidResponse)
    }

    pub async fn updates(
        &self,
        base: &str,
        token: &str,
        cursor: &str,
    ) -> Result<Updates, WeixinError> {
        let body = match self
            .post(
                base,
                "ilink/bot/getupdates",
                Some(token),
                json!({"get_updates_buf":cursor,"base_info":self.base_info()}),
                Duration::from_secs(35),
            )
            .await
        {
            Err(WeixinError::Timeout) => return Ok(Updates::default()),
            result => result?,
        };
        serde_json::from_value(body).map_err(|_| WeixinError::InvalidResponse)
    }

    pub async fn send_text(
        &self,
        base: &str,
        token: &str,
        recipient: &str,
        context: &str,
        client_id: &str,
        text: &str,
    ) -> Result<(), WeixinError> {
        if context.is_empty() {
            return Err(WeixinError::MissingContext);
        }
        let body = text_message(recipient, context, client_id, text, self.base_info());
        self.post(
            base,
            "ilink/bot/sendmessage",
            Some(token),
            body,
            Duration::from_secs(15),
        )
        .await?;
        Ok(())
    }
}

pub fn text_message(
    recipient: &str,
    context: &str,
    client_id: &str,
    text: &str,
    base_info: Value,
) -> Value {
    json!({"msg":{"from_user_id":"","to_user_id":recipient,"client_id":client_id,"message_type":2,"message_state":2,"context_token":context,"item_list":[{"type":1,"text_item":{"text":text}}]},"base_info":base_info})
}

fn check_response(body: &Value) -> Result<(), WeixinError> {
    if ["ret", "errcode"]
        .iter()
        .any(|key| body.get(key).and_then(Value::as_i64) == Some(-14))
    {
        return Err(WeixinError::Api(-14));
    }
    for key in ["ret", "errcode"] {
        if let Some(code) = body
            .get(key)
            .and_then(Value::as_i64)
            .filter(|code| *code != 0)
        {
            return Err(WeixinError::Api(code));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ilink_headers_payload_and_trusted_redirects() {
        let client = WeixinClient::new("https://ilinkai.weixin.qq.com", "2.4.9").unwrap();
        assert_eq!(version_number("2.4.9").unwrap(), 132105);
        let headers = client.headers(true, Some("secret"));
        assert_eq!(headers["AuthorizationType"], "ilink_bot_token");
        let decoded = STANDARD
            .decode(headers["X-WECHAT-UIN"].to_str().unwrap())
            .unwrap();
        assert!(String::from_utf8(decoded).unwrap().parse::<u32>().is_ok());
        assert!(
            !client
                .headers(false, None)
                .contains_key("AuthorizationType")
        );
        assert!(client.validated_base("https://other.weixin.qq.com").is_ok());
        for invalid in [
            "http://ilinkai.weixin.qq.com",
            "https://weixin.qq.com.evil.test",
            "http://127.0.0.1",
            "https://secret@ilinkai.weixin.qq.com",
            "https://ilinkai.weixin.qq.com/x",
        ] {
            assert!(client.validated_base(invalid).is_err());
        }
        let body = text_message("user", "context", "stable-id", "成功", json!({}));
        assert_eq!(body["msg"]["message_type"], 2);
        assert_eq!(body["msg"]["context_token"], "context");
        assert!(matches!(
            check_response(&json!({"ret":0,"errcode":-14})),
            Err(WeixinError::Api(-14))
        ));
        assert!(matches!(
            check_response(&json!({"ret":-1,"errcode":-14})),
            Err(WeixinError::Api(-14))
        ));
    }
}
