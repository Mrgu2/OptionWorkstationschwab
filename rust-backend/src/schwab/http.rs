async fn oauth_token_request(
    client_id: &str,
    client_secret: &str,
    form: &[(&str, &str)],
) -> anyhow::Result<TokenResponse> {
    let basic = format!("{client_id}:{client_secret}");
    let value = curl_json("POST", OAUTH_TOKEN_URL, None, Some(&basic), form).await?;
    serde_json::from_value(value).context("decode Schwab OAuth token response")
}

async fn curl_json(
    method: &str,
    url: &str,
    bearer: Option<&str>,
    basic: Option<&str>,
    form: &[(&str, &str)],
) -> anyhow::Result<Value> {
    let method = method.to_string();
    let url = url.to_string();
    let bearer = bearer.map(str::to_string);
    let basic = basic.map(str::to_string);
    let form: Vec<(String, String)> = form
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();

    tokio::task::spawn_blocking(move || {
        let mut command = Command::new("curl");
        command.args([
            "--silent",
            "--show-error",
            "--location",
            "--connect-timeout",
            "5",
            "--max-time",
            "15",
            "--request",
            &method,
        ]);

        if let Some(token) = bearer {
            command.args(["--header", &format!("Authorization: Bearer {token}")]);
        }
        if let Some(credentials) = basic {
            command.args(["--user", &credentials]);
        }
        for (key, value) in form {
            command.args(["--data-urlencode", &format!("{key}={value}")]);
        }
        command.args(["--write-out", "\n%{http_code}", &url]);

        let output = command.output().context("run curl for Schwab API")?;
        anyhow::ensure!(
            output.status.success(),
            "curl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );

        let stdout =
            String::from_utf8(output.stdout).context("Schwab API returned non UTF-8 response")?;
        let (body, status) = stdout
            .rsplit_once('\n')
            .ok_or_else(|| anyhow!("Schwab API response missing HTTP status"))?;
        let status: u16 = status.trim().parse().context("parse Schwab HTTP status")?;

        if status == 429 {
            return Err(anyhow!(
                "{RETRY_MARKER}5000; Schwab API rate limit reached"
            ));
        }
        anyhow::ensure!(
            (200..300).contains(&status),
            "Schwab API {status}: {}",
            compact_error(body)
        );

        if body.trim().is_empty() {
            Ok(Value::Null)
        } else {
            serde_json::from_str(body).context("decode Schwab JSON response")
        }
    })
    .await
    .context("join Schwab curl task")?
}

fn extract_authorization_code(value: &str) -> anyhow::Result<String> {
    let clean = value.trim();
    if !(clean.starts_with("http://") || clean.starts_with("https://")) {
        anyhow::ensure!(
            !clean.is_empty() && clean.len() <= 4096,
            "authorization code 长度异常"
        );
        return Ok(clean.to_string());
    }

    let query = clean
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or_default()
        .split('#')
        .next()
        .unwrap_or_default();
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "error" {
            return Err(anyhow!(
                "Schwab OAuth 返回错误: {}",
                percent_decode(value)
            ));
        }
        if key == "code" {
            return Ok(percent_decode(value));
        }
    }
    Err(anyhow!("回调 URL 中没有 code 参数"))
}

fn normalize_us_symbol(value: &str) -> anyhow::Result<String> {
    let clean = value
        .trim()
        .to_uppercase()
        .trim_end_matches(".US")
        .to_string();
    anyhow::ensure!(
        !clean.is_empty()
            && clean.len() <= 20
            && clean
                .chars()
                .all(|character| character.is_ascii_alphanumeric()
                    || matches!(character, '.' | '-')),
        "invalid symbol"
    );
    Ok(clean)
}

fn normalize_iv(value: f64) -> Option<f64> {
    let normalized = if value > 4.0 { value / 100.0 } else { value };
    (0.001..=4.0).contains(&normalized).then_some(normalized)
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|value| value as f64))
        .or_else(|| value.as_u64().map(|value| value as f64))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .filter(|value| value.is_finite())
}

fn integer(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_f64().map(|value| value.round() as i64))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn epoch_ms(value: &Value) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(integer(value)?).single()
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn compact_error(value: &str) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > 500 {
        format!("{}…", compact.chars().take(500).collect::<String>())
    } else if compact.is_empty() {
        "empty response".into()
    } else {
        compact
    }
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_value(bytes[index + 1]), hex_value(bytes[index + 2]))
        {
            output.push(high * 16 + low);
            index += 3;
            continue;
        }
        output.push(if bytes[index] == b'+' { b' ' } else { bytes[index] });
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn option_retry_after_ms(detail: &str) -> Option<u64> {
    let start = detail.find(RETRY_MARKER)? + RETRY_MARKER.len();
    let digits: String = detail[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        extract_authorization_code, normalize_iv, normalize_us_symbol, percent_decode,
        percent_encode,
    };

    #[test]
    fn extracts_code_from_callback_url() {
        assert_eq!(
            extract_authorization_code("https://127.0.0.1:5556/?code=abc%20123").unwrap(),
            "abc 123"
        );
    }

    #[test]
    fn normalizes_symbols_for_schwab() {
        assert_eq!(normalize_us_symbol("spy.us").unwrap(), "SPY");
    }

    #[test]
    fn normalizes_percent_iv() {
        assert_eq!(normalize_iv(25.0), Some(0.25));
    }

    #[test]
    fn encodes_redirect_uri() {
        assert!(percent_encode("https://127.0.0.1:5556").contains("%3A%2F%2F"));
    }

    #[test]
    fn decodes_url_values() {
        assert_eq!(percent_decode("abc%2Fdef+ghi"), "abc/def ghi");
    }
}
