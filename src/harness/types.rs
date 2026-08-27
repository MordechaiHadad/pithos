use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MountType {
    Credentials,
    State,
    Config,
    Ephemeral,
    Generated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Ro,
    Pinned,
    PinnedDir,
    Tmpfs,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Translation {
    #[default]
    None,
    PassthroughEnv,
    ClaudeSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostBase {
    Home,
    Data(String),
    State(String),
    Runtime,
}

impl<'de> Deserialize<'de> for HostBase {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        if raw == "home" {
            Ok(Self::Home)
        } else if raw == "runtime" {
            Ok(Self::Runtime)
        } else if let Some(rest) = raw.strip_prefix("data:") {
            if rest.is_empty() {
                return Err(serde::de::Error::custom(
                    "data host_base requires an application",
                ));
            }
            Ok(Self::Data(rest.to_string()))
        } else if let Some(rest) = raw.strip_prefix("state:") {
            if rest.is_empty() {
                return Err(serde::de::Error::custom(
                    "state host_base requires an application",
                ));
            }
            Ok(Self::State(rest.to_string()))
        } else {
            Err(serde::de::Error::unknown_variant(
                &raw,
                &[
                    "home",
                    "runtime",
                    "data:<application>",
                    "state:<application>",
                ][..],
            ))
        }
    }
}
