use reqwest::{
    header::{HeaderValue, AUTHORIZATION},
    Client,
};
use serde::Serialize;
use serde_json::Value;
use std::time::{Duration, Instant};

const MAX_BODY: usize = 2 * 1024 * 1024;

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    Timeout,
    Transport,
    BodyRead,
    BodyLimit,
}

#[derive(Debug, Serialize)]
pub struct Exchange {
    pub status: Option<u16>,
    pub elapsed_ms: u128,
    /// Exact bytes, or a bounded prefix if failure is BodyRead/BodyLimit.
    pub body: Vec<u8>,
    pub failure: Option<Failure>,
}

pub struct Transport {
    client: Client,
}

impl Transport {
    pub fn new(timeout: Duration) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Client::builder()
                .timeout(timeout)
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
        })
    }

    /// No auth/header values or request-builder error text enter the evidence.
    pub async fn exchange(
        &self,
        url: &str,
        request: Option<&Value>,
        authorization: Option<HeaderValue>,
    ) -> Exchange {
        let start = Instant::now();
        let mut builder = match request {
            Some(body) => self.client.post(url).json(body),
            None => self.client.get(url),
        };
        if let Some(header) = authorization {
            builder = builder.header(AUTHORIZATION, header);
        }
        let mut observation = Exchange {
            status: None,
            elapsed_ms: 0,
            body: Vec::new(),
            failure: None,
        };
        match builder.send().await {
            Err(error) => {
                observation.failure = Some(if error.is_timeout() {
                    Failure::Timeout
                } else {
                    Failure::Transport
                })
            }
            Ok(mut response) => {
                observation.status = Some(response.status().as_u16());
                loop {
                    match response.chunk().await {
                        Ok(Some(chunk)) => {
                            let remaining = MAX_BODY - observation.body.len();
                            observation
                                .body
                                .extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                            if chunk.len() > remaining {
                                observation.failure = Some(Failure::BodyLimit);
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            observation.failure = Some(if error.is_timeout() {
                                Failure::Timeout
                            } else {
                                Failure::BodyRead
                            });
                            break;
                        }
                    }
                }
            }
        }
        observation.elapsed_ms = start.elapsed().as_millis();
        observation
    }
}

pub fn bearer(key: &str) -> Result<HeaderValue, &'static str> {
    if key.trim().is_empty() {
        return Err("TYPESAFE_API_KEY is empty");
    }
    let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
        .map_err(|_| "TYPESAFE_API_KEY is not a valid header value")?;
    value.set_sensitive(true);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    fn server(status: &str, body: &str, delay: Duration) -> (String, thread::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let reply = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nLocation: {url}/redirected\r\n\r\n{body}", body.len());
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut received = Vec::new();
            let mut buffer = [0; 1024];
            while !received.windows(4).any(|w| w == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                received.extend_from_slice(&buffer[..count]);
            }
            thread::sleep(delay);
            let _ = stream.write_all(reply.as_bytes());
            listener.set_nonblocking(true).unwrap();
            1 + usize::from(listener.accept().is_ok())
        });
        (url, task)
    }

    #[tokio::test]
    async fn preserves_http_failures_and_does_not_retry_or_follow_redirects() {
        for status in [
            "401 Unauthorized",
            "422 Unprocessable Entity",
            "429 Too Many Requests",
            "529 Overloaded",
            "302 Found",
        ] {
            let (url, task) = server(status, "not necessarily JSON", Duration::ZERO);
            let result = Transport::new(Duration::from_secs(2))
                .unwrap()
                .exchange(&url, None, None)
                .await;
            assert_eq!(result.status, Some(status[..3].parse().unwrap()));
            assert_eq!(result.body, b"not necessarily JSON");
            assert_eq!(result.failure, None);
            assert_eq!(task.join().unwrap(), 1);
        }
    }

    #[tokio::test]
    async fn oversized_body_is_a_bounded_incomplete_observation() {
        let body = "x".repeat(MAX_BODY + 1);
        let (url, task) = server("200 OK", &body, Duration::ZERO);
        let result = Transport::new(Duration::from_secs(2))
            .unwrap()
            .exchange(&url, None, None)
            .await;
        assert_eq!(result.status, Some(200));
        assert_eq!(result.body.len(), MAX_BODY);
        assert_eq!(result.failure, Some(Failure::BodyLimit));
        task.join().unwrap();
    }

    #[tokio::test]
    async fn timeout_is_recorded_without_error_strings() {
        let (url, task) = server("200 OK", "{}", Duration::from_millis(100));
        let result = Transport::new(Duration::from_millis(30))
            .unwrap()
            .exchange(&url, None, None)
            .await;
        assert_eq!(result.failure, Some(Failure::Timeout));
        task.join().unwrap();
    }

    #[test]
    fn credentials_are_sensitive_and_bad_headers_do_not_echo_secrets() {
        assert!(bearer("test-secret").unwrap().is_sensitive());
        assert!(!bearer("secret\nvalue").unwrap_err().contains("secret"));
        assert!(bearer("").is_err());
    }
}
