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
    /// The page notes are written on: how many characters a line holds.
    pub page: Option<PageSettingsContent>,
    /// Whether the side panels tuck away while you type, and how they come
    /// back; `false` keeps them where they are.
    pub tides: Option<TidesContent>,
}

/// The page's width in characters of the body font (V54 §6).
#[with_fallible_options]
#[derive(Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, MergeFrom, Debug)]
pub struct PageSettingsContent {
    /// Characters per line of the page, in the body font.
    ///
    /// Default: 68
    pub measure: Option<u32>,
    /// The page narrows to this before an agent thread floats over it.
    ///
    /// Default: 48
    pub min_measure: Option<u32>,
}

/// `"tides": false`, or how the tides behave.
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom, Debug)]
#[serde(untagged)]
pub enum TidesContent {
    Enabled(bool),
    Settings(TidesSettingsContent),
}

/// What each side does while you type, and when it comes back (V54 §6).
/// A `tides` block replaces the default one whole, so a missing key is the
/// default.
#[derive(Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema, MergeFrom, Debug)]
pub struct TidesSettingsContent {
    /// "ebb" tucks the left side away while you type; "stay" keeps it open.
    ///
    /// Default: "ebb"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<TidePolicy>,
    /// "ebb" tucks the right side away while you type; "stay" keeps it open.
    ///
    /// Default: "ebb"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<TidePolicy>,
    /// Seconds without typing before tucked-away panels come back, or null
    /// for never.
    ///
    /// Default: 8
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_flow_back_after"
    )]
    pub flow_back_after: Option<FlowBackAfter>,
}

/// When tucked-away panels come back by themselves.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom)]
#[serde(untagged)]
pub enum FlowBackAfter {
    /// After this many seconds without typing.
    Seconds(u64),
    /// Never: only when asked. Written as `null`.
    Never,
}

/// A present `null` is `Never`, not a missing key.
fn deserialize_flow_back_after<'de, D>(deserializer: D) -> Result<Option<FlowBackAfter>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    FlowBackAfter::deserialize(deserializer).map(Some)
}

/// Whether a side tucks away while you type.
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
pub enum TidePolicy {
    /// Tucks away while you type, and comes back when you pause.
    #[default]
    Ebb,
    /// Stays open.
    Stay,
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
