use client::URL_SCHEME;
use gpui::{AsyncApp, actions};

actions!(
    cli,
    [
        /// Makes Thock the app that opens thock:// links.
        #[action(deprecated_aliases = ["cli::RegisterZedScheme"])]
        RegisterUrlScheme
    ]
);

pub async fn register_url_scheme(cx: &AsyncApp) -> anyhow::Result<()> {
    cx.update(|cx| cx.register_url_scheme(URL_SCHEME)).await
}
