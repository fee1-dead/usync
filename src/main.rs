use std::collections::HashMap;
use std::fs;
use std::sync::Arc;

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, post, web};
use dashmap::DashMap;
use parser::SyncSource;
use serde::Deserialize;
use tokio::sync::mpsc::{self, Sender};

mod parser;
mod sorter;
mod wp;

pub struct SharedState {
    map: DashMap<SyncSource, String>,
    client: mw::Client,
}

pub struct State {
    shared: Arc<SharedState>,
    sort: Sender<GitHubPush>,
}

pub enum SupportedWiki {
    Enwiki,
}

pub struct SyncConfig {
    pub repos: HashMap<String, Repo>,
}

pub struct Repo {
    pub files: HashMap<String, File>,
}

pub struct File {
    pub wiki: SupportedWiki,
    pub page_id: u64,
}

pub enum Commits {
    /// commit message
    Single(String),
    /// number of commits
    Multiple(usize),
}

pub struct Push {
    commits: Commits,
    authors: Vec<String>,
    url: String,
}

#[derive(Deserialize)]
struct GitHubAuthor {
    name: String,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Deserialize)]
struct GitHubCommit {
    author: GitHubAuthor,
    committer: GitHubAuthor,
    message: String,
    added: Vec<String>,
    modified: Vec<String>,
    id: String,
    url: String,
}

#[derive(Deserialize)]
struct GitHubPush {
    compare: String,
    commits: Vec<GitHubCommit>,
    pusher: GitHubAuthor,
    #[serde(rename = "ref")]
    r#ref: String,
    repository: String,
}

#[post("/")]
async fn handle(state: web::Data<State>, req: HttpRequest, body: String) -> impl Responder {
    let Some(val) = req.headers().get("X-GitHub-Event") else {
        return HttpResponse::ImATeapot().finish();
    };

    if val != "push" {
        return HttpResponse::Ok().finish();
    }

    let Ok(push) = serde_json::from_str::<GitHubPush>(&body) else {
        return HttpResponse::ImATeapot().finish();
    };

    if let Err(e) = state.sort.try_send(push) {
        tracing::error!(?e, "cannot send to sorter!");
        return HttpResponse::ImATeapot().finish();
    }

    HttpResponse::Ok().finish()
}

#[derive(Deserialize)]
pub struct Secrets {
    oauth_token: String,
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let secrets = fs::read_to_string("./secrets.toml")?;
    let secrets: Secrets = toml::from_str(&secrets)?;
    let (client, _) = mw::ClientBuilder::new("https://en.wikipedia.org/w/api.php")
        .login_oauth(&secrets.oauth_token)
        .await?;

    let (sort_send, sort_recv) = mpsc::channel(10);
    let shared = Arc::new(SharedState {
        map: DashMap::new(),
        client,
    });
    let data = web::Data::new(State {
        sort: sort_send,
        shared: shared.clone(),
    });

    HttpServer::new(move || App::new().app_data(data.clone()).service(handle))
        .bind(("0.0.0.0", 8000))?
        .run()
        .await?;

    Ok(())
}
