use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

const INDEX: &str = include_str!("../ui/index.html");
const APPJS: &str = include_str!("../ui/app.js");

pub async fn index() -> Html<&'static str> {
    Html(INDEX)
}

pub async fn app_js() -> Response {
    let mut r = APPJS.into_response();
    r.headers_mut().insert("content-type", "application/javascript".parse().unwrap());
    r
}

pub async fn favicon() -> Response {
    StatusCode::NO_CONTENT.into_response()
}
