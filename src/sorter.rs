use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::mpsc::Receiver;

use tokio::sync::mpsc::Sender;

use crate::parser::SyncSource;
use crate::Commits;
use crate::SharedState;
use crate::{GitHubPush, Push};

pub struct Context {
    pub ss: Arc<SharedState>,
    pub recv: Receiver<GitHubPush>,
    pub reparse_request: Sender<()>,
    pub send: Sender<Push>,
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
 
pub fn parse_wikitext_header(s: String) {

}

pub fn start(mut cx: Context) {
    tokio::spawn(async move { while let Some(push) = cx.recv.recv().await {
        if let Some(config) = cx.ss.map.get(&SyncSource {
            repo: push.repository.html_url.clone(),
            ref_: push.ref_.clone(),
        }) {
            if let Ok(text) = crate::wp::fetch(&cx.ss, &*config).await {
                
            }
        } else {
            cx.reparse_request.send(()).await.unwrap();
        }
    } });
}
