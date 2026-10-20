use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};

/// Thock's own settings: the faces a vault note is set in while its Markdown
/// is concealed.
#[with_fallible_options]
#[derive(Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, MergeFrom, Debug)]
pub struct ThockSettingsContent {
    /// The font family notes are set in, or "buffer" to use your editor font
    /// (`buffer_font_family`).
    ///
    /// Default: "IBM Plex Sans"
    pub body_font: Option<String>,
    /// The font family `#` and `##` headings are drawn in, or "body" to use
    /// the body font.
    ///
    /// Default: "Petrona"
    pub heading_font: Option<String>,
}
