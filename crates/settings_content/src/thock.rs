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
    /// How often Thock may start a conversation on its own: rarely, or
    /// never.
    ///
    /// Default: "rare"
    pub openers: Option<OpenerFrequency>,
}

/// How often Thock may start a conversation on its own (V44 §5.8).
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum OpenerFrequency {
    /// Only when it has something worth interrupting for, and never twice
    /// in a row.
    #[default]
    Rare,
    /// Never.
    Off,
}
