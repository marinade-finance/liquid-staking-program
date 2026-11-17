use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("cluster-profile")
        .build()?)
}

fn retry<T>(what: &str, mut f: impl FnMut() -> Result<(Option<T>, Option<String>), anyhow::Error>) -> Result<Option<T>> {
    // f returns Ok((Some(v),_)) = success, Ok((None, Some(reason))) = retryable, Err = fatal
    let mut delay = Duration::from_secs(1);
    for attempt in 1..=7 {
        match f() {
            Ok((Some(v), _)) => return Ok(Some(v)),
            Ok((None, None)) => return Ok(None),
            Ok((None, Some(reason))) => {
                if attempt == 7 {
                    bail!("{what}: giving up after {attempt} attempts: {reason}");
                }
                eprintln!("  {what}: {reason}, retry in {}s", delay.as_secs());
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_secs(20));
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

/// GET text. Ok(None) on 404.
pub fn http_get(url: &str) -> Result<Option<String>> {
    let c = client()?;
    retry(url, || {
        match c.get(url).send() {
            Ok(r) => {
                let s = r.status();
                if s.as_u16() == 404 {
                    return Ok((None, None));
                }
                if s.as_u16() == 429 || s.is_server_error() {
                    return Ok((None, Some(format!("HTTP {s}"))));
                }
                if !s.is_success() {
                    bail!("GET {url}: HTTP {s}");
                }
                Ok((Some(r.text()?), None))
            }
            Err(e) => Ok((None, Some(e.to_string()))),
        }
    })
}

pub struct Rpc {
    url: String,
    c: reqwest::blocking::Client,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Account {
    pub owner: String,
    pub lamports: u64,
    pub executable: bool,
    pub data: Vec<u8>,
}

impl Rpc {
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self { url: url.to_string(), c: client()? })
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let v = retry(&format!("rpc {method}"), || match self.c.post(&self.url).header("content-type", "application/json").body(body.to_string()).send() {
            Ok(r) => {
                let s = r.status();
                if s.as_u16() == 429 || s.is_server_error() {
                    return Ok((None, Some(format!("HTTP {s}"))));
                }
                if !s.is_success() {
                    bail!("rpc {method}: HTTP {s}");
                }
                let v: Value = serde_json::from_str(&r.text()?).context("decoding rpc response")?;
                if let Some(e) = v.get("error") {
                    let code = e.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                    if code == 429 || code == -32005 {
                        return Ok((None, Some(format!("rpc error {e}"))));
                    }
                    bail!("rpc {method} error: {e}");
                }
                Ok((Some(v), None))
            }
            Err(e) => Ok((None, Some(e.to_string()))),
        })?
        .ok_or_else(|| anyhow!("rpc {method}: no response"))?;
        Ok(v["result"].clone())
    }

    pub fn get_version(&self) -> Result<Value> {
        self.call("getVersion", json!([]))
    }
    pub fn get_epoch_info(&self) -> Result<Value> {
        self.call("getEpochInfo", json!([{"commitment":"confirmed"}]))
    }

    pub fn get_multiple(&self, keys: &[String]) -> Result<Vec<Option<Account>>> {
        let mut out = Vec::with_capacity(keys.len());
        for chunk in keys.chunks(100) {
            let r = self.call(
                "getMultipleAccounts",
                json!([chunk, {"encoding":"base64","commitment":"confirmed"}]),
            )?;
            let vals = r["value"].as_array().ok_or_else(|| anyhow!("getMultipleAccounts: bad response"))?;
            if vals.len() != chunk.len() {
                bail!("getMultipleAccounts: expected {} values, got {}", chunk.len(), vals.len());
            }
            for v in vals {
                if v.is_null() {
                    out.push(None);
                    continue;
                }
                let b64 = v["data"][0].as_str().ok_or_else(|| anyhow!("account without base64 data"))?;
                out.push(Some(Account {
                    owner: v["owner"].as_str().unwrap_or_default().to_string(),
                    lamports: v["lamports"].as_u64().unwrap_or(0),
                    executable: v["executable"].as_bool().unwrap_or(false),
                    data: base64::engine::general_purpose::STANDARD.decode(b64)?,
                }));
            }
        }
        Ok(out)
    }
}
