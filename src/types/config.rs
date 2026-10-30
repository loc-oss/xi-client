use serde::Deserialize;

#[derive(Default, Deserialize, Debug, PartialEq, Clone)]
pub struct Config {
    #[serde(default)]
    pub tab_size: Option<u64>,
    #[serde(default)]
    pub font_face: Option<String>,
    #[serde(default)]
    pub font_size: Option<f32>,
    #[serde(default)]
    pub translate_tabs_to_spaces: Option<bool>,
    #[serde(default)]
    pub word_wrap: Option<bool>,
    #[serde(default)]
    pub line_ending: Option<String>,
}
