use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::mpsc::Receiver;

use crate::SharedState;

#[derive(Deserialize)]
struct Slot {
    contentmodel: String,
    content: String,
}

#[derive(Deserialize)]
struct Slots {
    main: Slot,
}

#[derive(Deserialize)]
struct Revision {
    slots: Slots,
}

#[derive(Deserialize)]
struct Page {
    title: String,
    revisions: [Revision; 1],
}

#[derive(Deserialize)]
struct Query {
    pages: Vec<Page>,
}

#[derive(Deserialize)]
struct Response {
    query: Query,
}

#[derive(Debug)]
struct PageInfo {
    title: String,
    contentmodel: String,
    content: String,
}

#[derive(Clone, Hash, PartialEq, Eq, Deserialize)]
pub struct SyncSource {
    pub repo: String,
    #[serde(rename = "ref")]
    pub ref_: String,
}

#[derive(Deserialize)]
struct Sync {
    source: SyncSource,
    target_title: String,
}

#[derive(Deserialize)]
struct Config {
    syncs: Vec<Sync>,
}

async fn search(client: &mw::Client) -> color_eyre::Result<HashMap<SyncSource, String>> {
    let mut stream = client.get_all(
        &[
            ("action", "query"),
            ("generator", "linkshere"),
            ("pageids", "79491367"),
            ("prop", "revisions"),
            ("rvprop", "content|contentmodel"),
            ("rvslots", "main"),
        ],
        |r: Response| {
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

    let mut syncs = HashMap::new();

    'outer: while let Some(item) = stream.next().await {
        let item = item?;
        // usr is "User:0xDeadbeef"
        let Some((usr, _)) = item.title.split_once('/') else {
            continue;
        };
        /* let Some(usr) = usr.strip_prefix("User:") else {
            // TODO does this scale to other languages?
            continue;
        }; */
        if item.contentmodel != "javascript" {
            continue;
        }
        let Some(json_start) = item.content.find('{') else {
            continue;
        };
        let Some(json_end) = item.content.rfind('}') else {
            continue;
        };

        let json = &item.content[json_start..=json_end];

        let Ok(cfg) = serde_json::from_str::<Config>(json) else {
            continue;
        };

        for sync in &cfg.syncs {
            if !sync.target_title.starts_with(usr)
                || sync.target_title.as_bytes().get(usr.len()) != Some(&b'/')
            {
                // target isn't under one's userspace. bail
                continue 'outer;
            }
        }

        for sync in cfg.syncs {
            syncs.insert(sync.source, sync.target_title);
        }
    }

    Ok(syncs)
}

pub struct Context {
    pub ss: Arc<SharedState>,
    pub reparse_recv: Receiver<()>,
}

pub async fn task(mut ctx: Context) {
    // passively update everything per hour
    let mut int = tokio::time::interval(Duration::from_secs(60*60));

    loop {
        tokio::select! {
            _ = int.tick() => (),
            Some(_) = ctx.reparse_recv.recv() => (),
            else => break,
        }

        if let Ok(res) = search(&ctx.ss.client).await {
            *ctx.ss.map.lock().unwrap() = res;
        }
    }
}

pub fn start(ctx: Context) {
    tokio::spawn(task(ctx));
}
