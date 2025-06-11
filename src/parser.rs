use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc::Receiver;
use tracing::debug;

use crate::SharedState;
use crate::updater::parse_js_header;
use crate::wp::MultiPageResponse;

#[derive(Debug)]
struct PageInfo {
    title: String,
    contentmodel: String,
    content: String,
}

#[derive(Clone, Hash, PartialEq, Eq, Deserialize, Debug)]
pub struct SyncSource {
    pub repo: String,
    #[serde(rename = "ref")]
    pub ref_: String,
}

async fn search(client: &w::Client) -> color_eyre::Result<HashMap<SyncSource, Vec<String>>> {
    let mut stream = client.get_all(
        &[
            ("action", "query"),
            ("generator", "transcludedin"),
            ("pageids", "80169683"),
            ("prop", "revisions"),
            ("rvprop", "content|contentmodel"),
            ("rvslots", "main"),
        ],
        |r: MultiPageResponse| {
            Ok(r.query
                .pages
                .into_iter()
                .map(|p| {
                    let [rev] = p.revisions;
                    PageInfo {
                        title: p.title,
                        contentmodel: rev.slots.main.contentmodel,
                        content: rev.slots.main.content,
                    }
                })
                .collect::<Vec<_>>())
        },
    );

    let mut syncs: HashMap<SyncSource, Vec<String>> = HashMap::new();

    while let Some(item) = stream.next().await {
        let item = item?;

        if item.contentmodel != "javascript" {
            continue;
        }

        let Some(header) = parse_js_header(&item.content) else {
            continue;
        };
        syncs
            .entry(SyncSource {
                repo: header.repo,
                ref_: header.ref_,
            })
            .or_default()
            .push(item.title);
    }

    Ok(syncs)
}

pub struct Context {
    pub ss: Arc<SharedState>,
    pub reparse_recv: Receiver<()>,
}

pub async fn task(mut ctx: Context) {
    // passively update everything per hour
    let mut int = tokio::time::interval(Duration::from_secs(60 * 60));

    loop {
        tokio::select! {
            _ = int.tick() => (),
            Some(_) = ctx.reparse_recv.recv() => (),
            else => break,
        }

        if let Ok(res) = search(&ctx.ss.client).await {
            debug!(?res, "parsed map");
            *ctx.ss.map.lock().unwrap() = res;
        }
    }
}

pub fn start(ctx: Context) {
    tokio::spawn(task(ctx));
}
