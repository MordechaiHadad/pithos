use eyre::Result;

use crate::config::Config;

pub(crate) fn init() -> Result<()> {
    Config::init()
}
