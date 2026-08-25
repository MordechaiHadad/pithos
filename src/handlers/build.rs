use eyre::Result;
use std::path::Path;

use crate::config::Config;
use crate::registry;
use crate::sandbox::sweep_orphans;

pub(crate) fn build(config_path: Option<&Path>, toolchain: Option<String>) -> Result<()> {
    let config = Config::load(config_path, toolchain)?;
    sweep_orphans(&registry::sandbox_paths())?;
    config.build_image()
}
