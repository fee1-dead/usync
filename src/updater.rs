use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures_util::future::try_join_all;
use tokio::sync::mpsc::Receiver;

use tokio::sync::mpsc::Sender;
use tokio::time::error::Elapsed;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::trace;
use tracing::warn;

use crate::Commits;
use crate::SharedState;
use crate::parser::SyncSource;
use crate::{GitHubPush, Push};

pub struct Context {
    pub ss: Arc<SharedState>,
    pub send: Sender<GitHubPush>,
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
    pub path: String,
}

pub fn parse_js_header(s: &str) -> Option<Header> {
    let it = s
        .lines()
        .next()?
        .trim()
        .strip_prefix("//")?
        .trim_start()
        .strip_prefix("{{Wikipedia:USync")?
        .strip_suffix("}}")?
        .trim()
        .split('|')
        .map(str::trim);

    let mut repo = None;
    let mut ref_ = None;
    let mut path = None;

    for frag in it {
        let Some((param, arg)) = frag.split_once('=') else {
            continue;
        };

        match param.trim() {
            "repo" => repo = Some(arg.trim().to_owned()),
            "ref" => ref_ = Some(arg.trim().to_owned()),
            "path" => path = Some(arg.trim().to_owned()),
            _ => {}
        }
    }

    Some(Header {
        repo: repo?,
        ref_: ref_?,
        path: path?,
    })
}

#[test]
fn test_header_parse() {
    let h = "// {{Wikipedia:USync | repo = https://github.com/fee1-dead/usync |ref = refs/heads/main |path=test.js}}";

    let h = parse_js_header(h);

    assert!(h.is_some());

    let h = h.unwrap();
    assert_eq!("https://github.com/fee1-dead/usync", h.repo);
    assert_eq!("refs/heads/main", h.ref_);
    assert_eq!("test.js", h.path);
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
        .retain(|c| c.added.contains(&header.path) || c.modified.contains(&header.path));
    // the file must have been modified on Git's side for us to trigger an update
    if push.commits.is_empty() {
        info!("not modified");
        return;
    }

    let Some(repo) = header.repo.strip_prefix("https://github.com/") else {
        warn!(?header.repo, "non github URL");
        return;
    };

    let repo = repo.strip_suffix('/').unwrap_or(repo);
    let path = &header.path;

    // e.g. https://api.github.com/repos/fee1-dead/usync/contents/test.js
    let file_url = format!("https://api.github.com/repos/{repo}/contents/{path}");
    let file_url2 = push
        .repository
        .contents_url
        .replace("{+path}", &header.path);

    if file_url != file_url2 {
        warn!(?file_url, ?file_url2, "urls mismatched");
        return;
    }

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
        let titles = {
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

        debug!(?titles, "titles");

        let Some(titles) = titles else {
            info!("no title obtained");
            cx.reparse_request.send(()).await.unwrap();

            // send the push event back for a retry. Make sure that we don't keep retrying in a loop though.
            if !push.retry {
                let mut push = push;
                push.retry = true;
                let sender = cx.send.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let _ = sender.send(push).await;
                });
            }

            continue;
        };

        let ss2 = cx.ss.clone();

        let tasks = titles.into_iter().map(move |title| {
            tokio::time::timeout(
                Duration::from_secs(10),
                sort(ss2.clone(), push.clone(), title),
            )
        });

        tokio::spawn(async move {
            if let Err(Elapsed { .. }) = try_join_all(tasks).await {
                tracing::error!("task timed out!");
            }
        });
    }
}

pub fn start(cx: Context) {
    tokio::spawn(task(cx));
}
