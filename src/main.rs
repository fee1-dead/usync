use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, post, web};
use parser::SyncSource;
use serde::Deserialize;
use tokio::sync::mpsc::{self, Sender};
use tracing::info;
use tracing_subscriber::EnvFilter;

mod parser;
mod updater;
mod wp;

struct SharedState {
    map: Mutex<HashMap<SyncSource, Vec<String>>>,
    client: w::Client,
    req: reqwest::Client,
}

struct State {
    sort: Sender<GitHubPush>,
}

enum Commits {
    /// commit message
    Single(String),
    /// number of commits
    Multiple(usize),
}

struct Push {
    commits: Commits,
    authors: Vec<String>,
    url: String,
}

impl Push {
    pub fn into_edit_summary(self) -> String {
        let author = match &*self.authors {
            [] => {
                unreachable!()
            }
            [list @ ..] if list.len() <= 3 => list.join(", "),
            [first, rest @ ..] => {
                format!("{first} and {} others", rest.len())
            }
        };

        let commit = match self.commits {
            Commits::Single(msg) => msg,
            Commits::Multiple(n) => format!("{n} commits"),
        };

        format!(
            "[[[Wikipedia:Bots/Requests for approval/DeadbeefBot II|BOT]]] {author}: {commit} ({})",
            self.url
        )
    }
}

#[derive(Deserialize, Clone, Debug)]
struct GitHubAuthor {
    name: String,
}

#[derive(Deserialize, Clone, Debug)]
struct GitHubCommit {
    author: GitHubAuthor,
    committer: GitHubAuthor,
    message: String,
    added: Vec<String>,
    modified: Vec<String>,
}

#[derive(Deserialize, Clone, Debug)]
struct Repository {
    html_url: String,
    contents_url: String,
}

#[derive(Deserialize, Clone, Debug)]
struct GitHubPush {
    compare: String,
    commits: Vec<GitHubCommit>,
    #[serde(rename = "ref")]
    ref_: String,
    repository: Repository,
}

#[post("/webhook")]
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

    // todo add discord layer https://docs.rs/tracing-layer-discord/latest/tracing_layer_discord/
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let secrets = fs::read_to_string("./secrets.toml")?;
    let secrets: Secrets = toml::from_str(&secrets)?;
    let (client, _) = w::ClientBuilder::new("https://en.wikipedia.org/w/api.php")
        .login_oauth(&secrets.oauth_token)
        .await?;

    let (sort_send, update_recv) = mpsc::channel(10);
    let (reparse_send, reparse_recv) = mpsc::channel(10);
    let shared = Arc::new(SharedState {
        map: Mutex::new(HashMap::new()),
        client,
        req: reqwest::ClientBuilder::new().use_rustls_tls().build()?,
    });
    let data = web::Data::new(State {
        sort: sort_send,
        // shared: shared.clone(),
    });

    let updaterctx = updater::Context {
        ss: shared.clone(),
        reparse_request: reparse_send,
        recv: update_recv,
    };
    updater::start(updaterctx);

    let parsectx = parser::Context {
        ss: shared.clone(),
        reparse_recv,
    };
    parser::start(parsectx);

    info!("started");

    HttpServer::new(move || App::new().app_data(data.clone()).service(handle))
        .bind(("0.0.0.0", 8000))?
        .run()
        .await?;

    Ok(())
}
