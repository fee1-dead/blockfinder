use std::sync::Arc;
use std::time::Duration;

use actix_web::http::StatusCode;
use actix_web::web::Bytes;
use actix_web::{App, HttpResponseBuilder, HttpServer, Responder, get, web};
use color_eyre::eyre::OptionExt;
use futures_util::future::join_all;
use serde::Deserialize;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::{self, Sender};
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;
use tracing_subscriber::EnvFilter;

struct State {
    client: mw::Client,
}

async fn find_impl(
    user: String,
    state: Arc<State>,
    stream: Sender<color_eyre::Result<Bytes>>,
) -> color_eyre::Result<()> {
    let v = state
        .client
        .get([
            ("action", "query"),
            ("meta", "globaluserinfo"),
            ("guiuser", &user),
            ("guiprop", "merged"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<serde_json::Value>()
        .await?;

    let mut futures = Vec::new();
    let semaphore = Arc::new(Semaphore::new(8));
    for v in v["query"]["globaluserinfo"]["merged"].as_array().unwrap() {
        let base = v["url"].as_str().unwrap().to_owned();
        let api = format!("{}/w/api.php", base);
        let u = format!("User:{user}");
        let state = state.clone();
        let semaphore = semaphore.clone();
        let stream = stream.clone();

        futures.push(tokio::spawn(async move {
            let s = semaphore.acquire().await?;
            let v = state
                .client
                .with_url(&api)
                .get([
                    ("action", "query"),
                    ("list", "logevents"),
                    ("letype", "block"),
                    ("letitle", &u),
                ])
                .send()
                .await?
                .error_for_status()?
                .json::<serde_json::Value>()
                .await?;

            drop(s);

            let events = v["query"]["logevents"]
                .as_array()
                .ok_or_eyre("no logevents found")?;

            if events.is_empty() {
                stream.send(Ok(format!("{base} - OK\n").into())).await?;
            } else {
                stream
                    .send(Ok(format!(
                        "Please check {base}/w/index.php?title=Special:Log/block&page={u}\n"
                    )
                    .into()))
                    .await?;
            }
            color_eyre::Result::<_>::Ok(())
        }));
    }

    join_all(futures).await;
    Ok(())
}


#[derive(Deserialize)]
struct Req {
    username: String,
}

#[get("/find")]
async fn find(state: web::Data<State>, username: web::Query<Req>) -> impl Responder {
    let (sender, receiver) = mpsc::channel(2);
    tokio::spawn(timeout(Duration::from_secs(5), async move {
        match find_impl(username.into_inner().username, state.into_inner(), sender).await {
            Ok(()) => {}
            Err(e) => {
                tracing::error!(?e, "finding");
            }
        }
    }));

    HttpResponseBuilder::new(StatusCode::OK)
        .content_type("text/plain; charset=utf-8")
        .streaming(ReceiverStream::new(receiver))
}
 
#[get("/")]
async fn index() -> impl Responder {
    HttpResponseBuilder::new(StatusCode::OK)
        .content_type("text/html; charset=utf-8")
        .body(include_str!("index.html"))
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let client = mw::ClientBuilder::new("https://meta.wikimedia.org/w/api.php")
        .user_agent("fee1-dead/mw examples/blockfinder")
        .anonymous()?;

    let data = web::Data::new(State { client });

    HttpServer::new(move || App::new().app_data(data.clone()).service(find).service(index))
        .bind(("0.0.0.0", 8000))?
        .run()
        .await?;

    Ok(())
}
