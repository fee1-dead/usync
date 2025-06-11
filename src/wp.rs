use color_eyre::eyre::Result;
use serde::Deserialize;

use crate::SharedState;

#[derive(Deserialize)]
pub struct MainSlot {
    pub content: String,
    pub contentmodel: String,
}

#[derive(Deserialize)]
pub struct Slots {
    pub main: MainSlot,
}

#[derive(Deserialize)]
pub struct Revision {
    pub slots: Slots,
}

#[derive(Deserialize)]
pub struct Page {
    pub title: String,
    pub revisions: [Revision; 1],
}

#[derive(Deserialize)]
pub struct Pages<P> {
    pub pages: P,
}

#[derive(Deserialize)]
pub struct Response<P> {
    pub query: Pages<P>,
}

pub type SinglePageResponse = Response<[Page; 1]>;
pub type MultiPageResponse = Response<Vec<Page>>;

pub async fn fetch(ss: &SharedState, title: &str) -> Result<String> {
    let r = ss
        .client
        .get([
            ("action", "query"),
            ("prop", "revisions"),
            ("titles", title),
            ("rvprop", "content"),
            ("rvslots", "main"),
            ("rvcontentformat-main", "text/javascript"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<SinglePageResponse>()
        .await?;
    let [
        Page {
            revisions: [rev],
            title: _,
        },
    ] = r.query.pages;
    Ok(rev.slots.main.content)
}
