use color_eyre::eyre::{Context, Result};
use keyring::Entry;

const SERVICE: &str = "engineer-tui";

pub fn load(account: &str) -> Result<Option<String>> {
    let entry = Entry::new(SERVICE, account).context("open keyring entry")?;
    match entry.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).context("read keyring"),
    }
}

pub fn store(account: &str, secret: &str) -> Result<()> {
    let entry = Entry::new(SERVICE, account).context("open keyring entry")?;
    entry.set_password(secret).context("write keyring")
}

pub fn delete(account: &str) -> Result<()> {
    let entry = Entry::new(SERVICE, account).context("open keyring entry")?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).context("delete keyring"),
    }
}
