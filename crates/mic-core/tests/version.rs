//! The version check over HTTP, against a server that answers once with `version.json`.

use mic_core::version::{Standing, check};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Serves `body` as JSON once; returns its URL.
async fn serve(body: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 1024];
        let _read: usize = stream.read(&mut request).await.unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{address}/version.json")
}

#[tokio::test]
async fn a_newer_version_published_is_said() {
    let url =
        serve(r#"{"latest": "99.0.0", "min_compatible": "0.1.0", "download": "/#download"}"#).await;
    assert_eq!(
        check(&url).await,
        Ok(Standing::Newer {
            latest: "99.0.0".to_owned(),
            download: format!("{}/#download", url.trim_end_matches("/version.json"))
        })
    );
}

#[tokio::test]
async fn a_site_that_isnt_there_is_an_error() {
    assert!(check("http://127.0.0.1:9/version.json").await.is_err());
}
