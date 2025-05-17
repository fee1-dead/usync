use color_eyre::eyre::Result;
use serde::Deserialize;

use crate::SharedState;

pub async fn fetch(ss: &SharedState, title: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct MainSlot {
        content: String,
    }
    #[derive(Deserialize)]
    struct Slots {
        main: MainSlot,
    }
    #[derive(Deserialize)]
    struct Revision {
        slots: Slots,
    }
    #[derive(Deserialize)]
    struct Page {
        revisions: [Revision; 1],
    }
    #[derive(Deserialize)]
    struct Pages {
        pages: [Page; 1],
    }
    #[derive(Deserialize)]
    struct Response {
        query: Pages,
    }
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
        .json::<Response>()
        .await?;
    let [Page { revisions: [rev] }] = r.query.pages;
    Ok(rev.slots.main.content)
}
