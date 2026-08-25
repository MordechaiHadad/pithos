use eyre::Result;
use std::path::Path;

use crate::config::Config;

pub(crate) fn build(config_path: Option<&Path>, toolchain: Option<String>) -> Result<()> {
    let config = Config::load(config_path, toolchain)?;
    config.build_image()
}
