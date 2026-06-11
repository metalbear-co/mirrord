use bytes::Bytes;
use http::{Method, Request, StatusCode, Uri, header};
use http_body_util::{BodyExt, Full};
use serde::{Serialize, de::DeserializeOwned};

use crate::{SessionClient, SessionError};

/// Minimal request builder for the session monitor transport.
///
/// This intentionally mirrors the small part of `reqwest::RequestBuilder` the UI proxy needs,
/// while still sending requests over the per-session Unix socket or named pipe.
pub struct RequestBuilder {
    client: SessionClient,
    method: Method,
    url: String,
    body: Result<Option<Bytes>, SessionError>,
    content_type: Option<&'static str>,
    accept: Option<&'static str>,
}

impl RequestBuilder {
    pub(crate) fn new(client: SessionClient, method: Method, url: String) -> Self {
        Self {
            client,
            method,
            url,
            body: Ok(None),
            content_type: None,
            accept: None,
        }
    }

    pub fn json<T>(mut self, value: &T) -> Self
    where
        T: Serialize + ?Sized,
    {
        self.body = serde_json::to_vec(value)
            .map(Bytes::from)
            .map(Some)
            .map_err(SessionError::Json);
        self.content_type = Some("application/json");
        self.accept = Some("application/json");
        self
    }

    pub async fn send(self) -> Result<Response, SessionError> {
        let body = self.body?.unwrap_or_default();
        let path = path_and_query(&self.url);

        let mut builder = Request::builder()
            .method(self.method)
            .uri(format!("http://localhost{path}"))
            .header(header::HOST, "localhost");

        if let Some(content_type) = self.content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }

        if let Some(accept) = self.accept {
            builder = builder.header(header::ACCEPT, accept);
        }

        let request = builder.body(Full::new(body)).map_err(SessionError::Build)?;

        let mut sender = self.client.open_h1_sender().await?;
        let response = sender
            .send_request(request)
            .await
            .map_err(SessionError::Request)?;

        Ok(Response(response))
    }
}

pub struct Response(http::Response<hyper::body::Incoming>);

impl Response {
    pub fn status(&self) -> StatusCode {
        self.0.status()
    }

    pub async fn bytes(self) -> Result<Bytes, SessionError> {
        Ok(self
            .0
            .into_body()
            .collect()
            .await
            .map_err(|fail| SessionError::Body(fail.to_string()))?
            .to_bytes())
    }

    pub async fn json<T>(self) -> Result<T, SessionError>
    where
        T: DeserializeOwned,
    {
        let bytes = self.bytes().await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

fn path_and_query(url: &str) -> String {
    url.parse::<Uri>()
        .ok()
        .and_then(|uri| uri.path_and_query().map(ToString::to_string))
        .filter(|path| path.starts_with('/'))
        .unwrap_or_else(|| {
            if url.starts_with('/') {
                url.to_owned()
            } else {
                format!("/{url}")
            }
        })
}
