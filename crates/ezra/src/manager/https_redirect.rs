use std::future::Future;
use std::io;
use std::pin::Pin;
use std::time::Duration;

use axum_server::accept::Accept;
use axum_server::tls_rustls::{RustlsAcceptor, RustlsConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// The first byte of every TLS connection, the handshake record type.
const TLS_HANDSHAKE: u8 = 0x16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long to wait for the rest of a request after answering it, so closing does not reset the
/// connection before the client reads the answer.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
const LONGEST_REQUEST_HEAD: u64 = 8 * 1024;
const BAD_REQUEST: &str = "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nContent-Length: 30\r\nConnection: close\r\n\r\nThis port only serves HTTPS.\r\n";

/// Serves HTTPS, and answers plain HTTP on the same port with a redirect to HTTPS.
#[derive(Clone)]
pub struct HttpsRedirectAcceptor {
    tls: RustlsAcceptor,
}

impl HttpsRedirectAcceptor {
    pub fn new(config: RustlsConfig) -> Self {
        Self {
            tls: RustlsAcceptor::new(config),
        }
    }
}

type Accepted<S> = io::Result<(<RustlsAcceptor as Accept<TcpStream, S>>::Stream, S)>;

impl<S: Send + 'static> Accept<TcpStream, S> for HttpsRedirectAcceptor {
    type Stream = <RustlsAcceptor as Accept<TcpStream, S>>::Stream;
    type Service = S;
    type Future = Pin<Box<dyn Future<Output = Accepted<S>> + Send>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        let tls = self.tls.clone();
        Box::pin(async move {
            let mut first_byte = [0; 1];
            let peeked = timeout(REQUEST_TIMEOUT, stream.peek(&mut first_byte)).await??;
            if peeked == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            if first_byte == [TLS_HANDSHAKE] {
                return tls.accept(stream, service).await;
            }
            timeout(REQUEST_TIMEOUT, stream.redirect_to_https()).await??;
            Err(io::Error::other("redirected a plain HTTP request to HTTPS"))
        })
    }
}

trait PlainHttpStreamExt {
    /// Reads a plain HTTP request and answers it with a redirect to HTTPS, or with 400 when it
    /// cannot be redirected.
    async fn redirect_to_https(self) -> io::Result<()>;
}

impl PlainHttpStreamExt for TcpStream {
    async fn redirect_to_https(mut self) -> io::Result<()> {
        let mut head = Vec::new();
        let response = loop {
            let read = (&mut self)
                .take(LONGEST_REQUEST_HEAD.saturating_sub(head.len() as u64))
                .read_buf(&mut head)
                .await?;
            if let Some(response) = head.https_redirect() {
                break response;
            }
            if read == 0 {
                break BAD_REQUEST.to_owned();
            }
        };
        self.write_all(response.as_bytes()).await?;
        self.shutdown().await?;
        let _ = timeout(
            DRAIN_TIMEOUT,
            tokio::io::copy(&mut self, &mut tokio::io::sink()),
        )
        .await;
        Ok(())
    }
}

trait RequestHeadExt {
    /// The answer to a plain HTTP request head, or `None` while the head is incomplete.
    fn https_redirect(&self) -> Option<String>;
}

impl RequestHeadExt for [u8] {
    fn https_redirect(&self) -> Option<String> {
        let mut headers = [httparse::EMPTY_HEADER; 64];
        let mut request = httparse::Request::new(&mut headers);
        match request.parse(self) {
            Ok(httparse::Status::Partial) => None,
            Ok(httparse::Status::Complete(_)) => Some(match request.https_location() {
                Some(location) => format!(
                    "HTTP/1.1 301 Moved Permanently\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                ),
                None => BAD_REQUEST.to_owned(),
            }),
            Err(_) => Some(BAD_REQUEST.to_owned()),
        }
    }
}

trait HttpRequestExt {
    /// The same host, port and path over HTTPS.
    fn https_location(&self) -> Option<String>;
}

impl HttpRequestExt for httparse::Request<'_, '_> {
    fn https_location(&self) -> Option<String> {
        let host = self
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("host"))
            .and_then(|header| std::str::from_utf8(header.value).ok())?
            .trim();
        let is_host_and_port = |character: char| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | ':' | '[' | ']')
        };
        if host.is_empty() || !host.chars().all(is_host_and_port) {
            return None;
        }
        let path = self.path.filter(|path| path.starts_with('/'))?;
        Some(format!("https://{host}{path}"))
    }
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr};

    use axum::Router;
    use axum::routing::get;
    use axum_server::Handle;

    use super::*;
    use crate::manager::tls::ServedCertificate;

    #[test]
    fn redirects_to_the_same_host_port_and_path() {
        let response = b"GET /agents?tab=codex HTTP/1.1\r\nHost: my-box:8443\r\n\r\n"
            .https_redirect()
            .expect("the request is complete");
        assert!(
            response.starts_with("HTTP/1.1 301 Moved Permanently\r\n"),
            "{response}"
        );
        assert!(
            response.contains("\r\nLocation: https://my-box:8443/agents?tab=codex\r\n"),
            "{response}"
        );
    }

    #[test]
    fn keeps_ipv6_hosts() {
        let response = b"GET / HTTP/1.1\r\nhost: [::1]:9443\r\n\r\n"
            .https_redirect()
            .expect("the request is complete");
        assert!(
            response.contains("\r\nLocation: https://[::1]:9443/\r\n"),
            "{response}"
        );
    }

    #[test]
    fn waits_for_the_whole_request_head() {
        assert_eq!(b"GET / HTTP/1.1\r\nHost: my-box".https_redirect(), None);
    }

    #[test]
    fn rejects_requests_it_cannot_redirect() {
        for head in [
            b"GET / HTTP/1.0\r\n\r\n".as_slice(),
            b"GET / HTTP/1.1\r\nHost: evil.example/@my-box\r\n\r\n",
            b"GET http://my-box/ HTTP/1.1\r\nHost: my-box\r\n\r\n",
            b"not http at all\r\n\r\n",
        ] {
            assert_eq!(
                head.https_redirect().as_deref(),
                Some(BAD_REQUEST),
                "{}",
                String::from_utf8_lossy(head)
            );
        }
    }

    #[test]
    fn bad_request_length_matches_its_body() {
        let (head, body) = BAD_REQUEST.split_once("\r\n\r\n").expect("head and body");
        assert!(head.contains(&format!("Content-Length: {}\r\n", body.len())));
    }

    /// Serves "ok" at `/` with the acceptor on a free port.
    async fn serve() -> (Handle<SocketAddr>, u16) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let directory = tempfile::tempdir().expect("temporary directory");
        let certificate = ServedCertificate::load(directory.path(), "ezra")
            .await
            .expect("certificate is served");
        let handle = Handle::new();
        let server = axum_server::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .acceptor(HttpsRedirectAcceptor::new(certificate.config.clone()))
            .handle(handle.clone())
            .serve(
                Router::new()
                    .route("/", get(|| async { "ok" }))
                    .into_make_service(),
            );
        tokio::spawn(server);
        let port = handle.listening().await.expect("server listens").port();
        (handle, port)
    }

    /// Sends `parts` over plain HTTP, a moment apart, and reads the whole response. With
    /// `hang_up`, the client stops sending after the last part.
    async fn plain_response(port: u16, parts: &[&[u8]], hang_up: bool) -> String {
        let mut plain = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("connects");
        for part in parts {
            plain.write_all(part).await.expect("request is sent");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if hang_up {
            plain.shutdown().await.expect("the client hangs up");
        }
        let mut response = String::new();
        plain
            .read_to_string(&mut response)
            .await
            .expect("response is read");
        response
    }

    #[tokio::test]
    async fn plain_http_is_redirected_and_https_is_served() {
        let (handle, port) = serve().await;

        let request = format!("GET /login HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n");
        let (first, rest) = request.as_bytes().split_at(10);
        let response = plain_response(port, &[first, rest], false).await;
        assert!(
            response.contains(&format!("\r\nLocation: https://localhost:{port}/login\r\n")),
            "{response}"
        );

        let client = reqwest::Client::builder()
            .tls_danger_accept_invalid_certs(true)
            .build()
            .expect("client builds");
        let response = client
            .get(format!("https://localhost:{port}/"))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .expect("HTTPS is served");
        assert_eq!(response.version(), reqwest::Version::HTTP_2);
        assert_eq!(response.text().await.expect("body is read"), "ok");
        handle.shutdown();
    }

    #[tokio::test]
    async fn unfinished_requests_are_rejected() {
        let (handle, port) = serve().await;
        let response = plain_response(port, &[b"GET / HTTP/1.1\r\nHost: local"], true).await;
        assert_eq!(response, BAD_REQUEST);
        handle.shutdown();
    }

    #[tokio::test]
    async fn oversized_requests_are_rejected() {
        let (handle, port) = serve().await;
        let oversized = format!(
            "GET / HTTP/1.1\r\nHost: localhost\r\nCookie: {}\r\n",
            "a".repeat(9 * 1024)
        );
        let response = plain_response(port, &[oversized.as_bytes()], false).await;
        assert_eq!(response, BAD_REQUEST);
        handle.shutdown();
    }
}
