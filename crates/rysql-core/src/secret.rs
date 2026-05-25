//! OS keyring wrapper for connection passwords.

use crate::{CoreError, Result};

const SERVICE: &str = "rysql";

fn entry(account: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, account).map_err(CoreError::from)
}

pub fn store_password(account: &str, password: &str) -> Result<()> {
    entry(account)?.set_password(password)?;
    Ok(())
}

pub fn fetch_password(account: &str) -> Result<Option<String>> {
    match entry(account)?.get_password() {
        Ok(p) => Ok(Some(p)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn delete_password(account: &str) -> Result<()> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
