use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use wiremock::{
    matchers::{header, method, path, query_param},
    Mock, MockServer, ResponseTemplate,
};

fn tokens() -> Value {
    json!({"code":0,"data":{"accessToken":"test-access","refreshToken":"test-refresh","expiresIn":3600,"domain":"tenant"}})
}
#[tokio::test]
async fn authorize_poll_account_and_refresh_protocol() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/auth/state"))
        .and(query_param("platform", "workbuddy"))
        .and(header("x-product", "SaaS"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"state":"test-state","authUrl":"https://www.codebuddy.cn/auth"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let polls = Arc::new(AtomicUsize::new(0));
    let count = polls.clone();
    Mock::given(method("GET"))
        .and(path("/auth/token"))
        .and(query_param("state", "test-state"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200).set_body_json(if count.fetch_add(1, Ordering::SeqCst) == 0 {
                json!({"code":11217})
            } else {
                tokens()
            })
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(path("/login/account"))
        .and(header("authorization", "Bearer test-access"))
        .and(header("x-domain", "tenant"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"data":{"uid":"account","nickname":"Account","enterpriseId":"enterprise"}}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let url = Arc::new(Mutex::new(String::new()));
    let capture = url.clone();
    let credential = authorize_at(&server.uri(), Duration::ZERO, move |v| {
        *capture.lock().unwrap() = v;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(*url.lock().unwrap(), "https://www.codebuddy.cn/auth");
    assert_eq!(credential["userId"], "account");
    assert_eq!(credential["enterpriseId"], "enterprise");
    Mock::given(method("POST"))
        .and(path("/auth/token/refresh"))
        .and(header("x-refresh-token", "test-refresh"))
        .and(header("x-auth-refresh-source", "workbuddy"))
        .and(header("x-user-id", "account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tokens()))
        .expect(1)
        .mount(&server)
        .await;
    let refreshed = refresh_at(&server.uri(), &credential).await.unwrap();
    assert_eq!(refreshed["userId"], "account");
    assert_eq!(refreshed["label"], "Account");
}
#[test]
fn malformed_and_rejected_credentials_do_not_pass() {
    assert!(credential(json!({"accessToken":"x","refreshToken":"y","expiresIn":0})).is_err());
    assert!(credential(json!({"accessToken":"x","expiresIn":3600})).is_err());
    assert!(poll_result(json!({"code":123,"message":"secret"}))
        .unwrap_err()
        .find("secret")
        .is_none());
    assert!(poll_result(json!({"code":"0","data":{}})).is_err());
    assert!(auth_state(json!({"state":"x","authUrl":"http://example.com"})).is_err());
    assert!(auth_state(json!({"state":"","authUrl":"https://example.com"})).is_err());
}
#[tokio::test]
async fn pending_poll_cancels_without_waiting_for_timeout() {
    let server = MockServer::start().await;
    Mock::given(path("/auth/state"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"state":"state","authUrl":"https://www.codebuddy.cn/auth"})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/auth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"code":11217})))
        .mount(&server)
        .await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        clone.cancel();
    });
    let result = super::super::cancellable(
        cancel,
        5,
        authorize_at(&server.uri(), Duration::from_secs(60), |_| Ok(())),
    )
    .await;
    assert!(result.unwrap_err().contains("cancelled"));
}
async fn stream_response(body: &str) -> Response {
    let server = MockServer::start().await;
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    reqwest::get(server.uri()).await.unwrap()
}
#[tokio::test]
async fn text_stream_is_incremental_and_requires_done() {
    let response = stream_response("data: {\"choices\":[{\"delta\":{\"content\":\"你\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"好\"}}]}\n\ndata: [DONE]\n\n").await;
    let chunks = Mutex::new(Vec::new());
    let output = consume_chat(response, |delta| {
        chunks.lock().unwrap().push(delta);
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(output, "你好");
    assert_eq!(*chunks.lock().unwrap(), vec!["你", "好"]);
    for body in [
        "data: {}\n\n",
        "data: not-json\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
    ] {
        assert!(consume_chat(stream_response(body).await, |_| Ok(()))
            .await
            .is_err());
    }
}
