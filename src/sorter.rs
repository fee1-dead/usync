use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::Receiver;

use tokio::sync::mpsc::Sender;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::trace;

use crate::Commits;
use crate::SharedState;
use crate::parser::SyncSource;
use crate::{GitHubPush, Push};

pub struct Context {
    pub ss: Arc<SharedState>,
    pub recv: Receiver<GitHubPush>,
    pub reparse_request: Sender<()>,
}

pub fn parse_webhook(p: GitHubPush) -> Push {
    let mut names = HashSet::new();
    let commits = if p.commits.len() == 1 {
        Commits::Single(p.commits.first().unwrap().message.clone())
    } else {
        Commits::Multiple(p.commits.len())
    };
    for commit in p.commits {
        names.insert(commit.author.name);
        names.insert(commit.committer.name);
    }

    Push {
        commits,
        authors: names.into_iter().collect(),
        url: p.compare,
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Header {
    pub repo: String,
    pub ref_: String,
    pub file: String,
}

pub fn parse_js_header(s: &str) -> Option<Header> {
    let mut it = s
        .strip_prefix("// [[User:0xDeadbeef/usync]]:")?
        .lines()
        .next()?
        .trim()
        .split_whitespace()
        .map(ToOwned::to_owned);
    let repo = it.next()?;
    let ref_ = it.next()?;
    let file = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(Header { repo, ref_, file })
}

pub async fn sort(ss: Arc<SharedState>, mut push: GitHubPush, title: String) {
    let Ok(orig_src) = crate::wp::fetch(&ss, &title).await else {
        error!("couldn't fetch");
        return;
    };
    // refetch the info on-wiki to compare
    let Some(header) = parse_js_header(&orig_src) else {
        error!("couldn't parse on-wiki header");
        return;
    };

    // check again that the reference and the repo url match
    if push.ref_ != header.ref_ || push.repository.html_url != header.repo {
        error!("2nd comparison failed");
        return;
    }

    push.commits
        .retain(|c| c.added.contains(&header.file) || c.modified.contains(&header.file));
    // the file must have been modified on Git's side for us to trigger an update
    if push.commits.is_empty() {
        info!("not modified");
        return;
    }

    // e.g. https://api.github.com/repos/fee1-dead/usync/contents/test.js
    let file_url = push
        .repository
        .contents_url
        .replace("{+path}", &header.file);

    // TODO: handle these errors and log
    let Ok(res) = ss
        .req
        .get(&file_url)
        .query(&[("ref", &header.ref_)])
        .header("Accept", "application/vnd.github.raw+json")
        .header("User-Agent", "fee1-dead/usync")
        .send()
        .await
    else {
        error!("couldn't get content from github");
        return;
    };

    let Ok(newtext) = res.text().await else {
        error!("couldn't get text from github");
        return;
    };
    trace!(%newtext, %orig_src);

    // no need to edit if nothing changed
    if newtext == orig_src {
        info!("nothing changed");
        return;
    }

    // ensure that the github side has the same header.
    if parse_js_header(&newtext) != Some(header) {
        info!("header mismatched");
        return;
    }

    let push = parse_webhook(push);

    let summary = push.into_edit_summary();

    let Ok(tok) = ss.client.get_token("csrf").await else {
        error!("couldn't get csrf token");
        return;
    };

    match ss
        .client
        .post([
            ("action", "edit"),
            ("title", &title),
            ("text", &newtext),
            ("summary", &summary),
            ("bot", "1"),
            ("nocreate", "1"),
            ("contentformat", "text/javascript"),
            ("contentmodel", "javascript"),
            ("token", &tok),
        ])
        .send()
        .await
    {
        Ok(res) => {
            debug!(?res);
            match res.error_for_status() {
                Err(e) => {
                    tracing::error!(?e, "status response for edit");
                }
                Ok(res) => {
                    let text = res.text().await;
                    debug!(?text);
                }
            }
        }
        Err(e) => {
            tracing::error!(?e, "edit");
        }
    }
}

pub async fn task(mut cx: Context) {
    while let Some(push) = cx.recv.recv().await {
        debug!(?push, "got task");
        // we must already know of an on-wiki sync file with the given repo and reference
        let config = {
            // be very careful as to not hold the lock for too long
            let lock = cx.ss.map.lock().unwrap();
            let config = lock
                .get(&SyncSource {
                    repo: push.repository.html_url.clone(),
                    ref_: push.ref_.clone(),
                })
                .map(ToOwned::to_owned);

            drop(lock);

            config
        };

        debug!(?config, "config");

        let Some(config) = config else {
            info!("no config obtained");
            cx.reparse_request.send(()).await.unwrap();
            continue;
        };

        let task = tokio::time::timeout(
            Duration::from_secs(5),
            sort(cx.ss.clone(), push, config.to_owned()),
        );
        tokio::spawn(async move {
            match task.await {
                Err(_) => {
                    tracing::error!("task timed out!");
                }
                Ok(()) => {}
            }
        });
    }
}

pub fn start(cx: Context) {
    tokio::spawn(task(cx));
}
