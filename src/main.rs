use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, Responder, post, web};
use parser::SyncSource;
use serde::Deserialize;
use tokio::sync::mpsc::{self, Sender};
use tracing_subscriber::EnvFilter;

mod parser;
mod sorter;
mod wp;

pub struct SharedState {
    map: Mutex<HashMap<SyncSource, String>>,
    client: mw::Client,
    req: reqwest::Client,
}

pub struct State {
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

        // TODO add BRFA link
        format!("{author}: {commit} ({})", self.url)
    }
}

#[derive(Deserialize, Debug)]
struct GitHubAuthor {
    name: String,
}

#[derive(Deserialize, Debug)]
struct GitHubCommit {
    author: GitHubAuthor,
    committer: GitHubAuthor,
    message: String,
    added: Vec<String>,
    modified: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct Repository {
    html_url: String,
}

#[derive(Deserialize, Debug)]
struct GitHubPush {
    compare: String,
    commits: Vec<GitHubCommit>,
    pusher: GitHubAuthor,
    #[serde(rename = "ref")]
    ref_: String,
    repository: Repository,
}

#[post("/")]
async fn handle(state: web::Data<State>, req: HttpRequest, body: String) -> impl Responder {
    println!("{body}");
    let Some(val) = req.headers().get("X-GitHub-Event") else {
        return HttpResponse::ImATeapot().finish();
    };

    if val != "push" {
        return HttpResponse::Ok().finish();
    }

    let Ok(push) = serde_json::from_str::<GitHubPush>(&body) else {
        return HttpResponse::ImATeapot().finish();
    };

    println!("{push:?}");

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
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let secrets = fs::read_to_string("./secrets.toml")?;
    let secrets: Secrets = toml::from_str(&secrets)?;
    let (client, _) = mw::ClientBuilder::new("https://en.wikipedia.org/w/api.php")
        .login_oauth(&secrets.oauth_token)
        .await?;

    let (sort_send, sort_recv) = mpsc::channel(10);
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

    let sortctx = sorter::Context {
        ss: shared.clone(),
        reparse_request: reparse_send,
        recv: sort_recv,
    };
    sorter::start(sortctx);

    let parsectx = parser::Context {
        ss: shared.clone(),
        reparse_recv,
    };
    parser::start(parsectx);

    HttpServer::new(move || App::new().app_data(data.clone()).service(handle))
        .bind(("0.0.0.0", 8000))?
        .run()
        .await?;

    Ok(())
}
