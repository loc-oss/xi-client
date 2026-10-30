use serde::{self, Deserialize, Deserializer};

#[derive(Default, Debug, PartialEq, Clone)]
pub struct StyleDef {
    pub offset: i64,
    pub length: u64,
    pub style_id: u64,
}

#[derive(Default, Deserialize, Debug, PartialEq, Clone)]
pub struct Line {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub cursor: Vec<u64>,
    #[serde(default, deserialize_with = "deserialize_styles")]
    pub styles: Vec<StyleDef>,
    #[serde(rename = "ln")]
    pub line_num: Option<u64>,
}

pub fn deserialize_styles<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<StyleDef>, D::Error> {
    let opt = Option::<Vec<i64>>::deserialize(d)?;
    let v = opt.unwrap_or_default();
    if v.len() % 3 != 0 {
        return Err(serde::de::Error::custom("styles length is not a multiple of 3"));
    }
    let mut out = Vec::with_capacity(v.len() / 3);
    for i in 0..v.len() / 3 {
        out.push(StyleDef {
            offset: v[i * 3],
            length: v[i * 3 + 1] as u64,
            style_id: v[i * 3 + 2] as u64,
        });
    }
    Ok(out)
}
