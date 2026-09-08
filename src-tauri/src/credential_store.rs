use fs2::FileExt;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::PathBuf,
};

#[derive(Debug)]
pub enum Error {
    NoEntry,
    Storage,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoEntry => "尚未保存凭据。",
            Self::Storage => "无法读写本地凭据文件。",
        })
    }
}
impl std::error::Error for Error {}

pub struct Entry {
    root: PathBuf,
    id: String,
}
impl Entry {
    pub fn new(root: PathBuf, id: &str) -> Result<Self, Error> {
        if id.is_empty() || id.len() > 512 {
            return Err(Error::Storage);
        }
        Ok(Self {
            root,
            id: id.into(),
        })
    }
    fn lock(&self) -> Result<File, Error> {
        fs::create_dir_all(&self.root).map_err(|_| Error::Storage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
                .map_err(|_| Error::Storage)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options
            .open(self.root.join("credentials.lock"))
            .map_err(|_| Error::Storage)?;
        lock.lock_exclusive().map_err(|_| Error::Storage)?;
        Ok(lock)
    }
    fn read(&self) -> Result<BTreeMap<String, String>, Error> {
        match fs::read(self.root.join("credentials.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| Error::Storage),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(_) => Err(Error::Storage),
        }
    }
    fn write(&self, values: &BTreeMap<String, String>) -> Result<(), Error> {
        let temporary = self
            .root
            .join(format!(".credentials-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary).map_err(|_| Error::Storage)?;
            let bytes = serde_json::to_vec(values).map_err(|_| Error::Storage)?;
            file.write_all(&bytes).map_err(|_| Error::Storage)?;
            file.sync_all().map_err(|_| Error::Storage)?;
            drop(file);
            fs::rename(&temporary, self.root.join("credentials.json")).map_err(|_| Error::Storage)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    pub fn get_password(&self) -> Result<String, Error> {
        let _guard = self.lock()?;
        self.read()?.remove(&self.id).ok_or(Error::NoEntry)
    }
    pub fn set_password(&self, secret: &str) -> Result<(), Error> {
        let _guard = self.lock()?;
        let mut values = self.read()?;
        values.insert(self.id.clone(), secret.into());
        self.write(&values)
    }
    pub fn delete_credential(&self) -> Result<(), Error> {
        let _guard = self.lock()?;
        let mut values = self.read()?;
        if values.remove(&self.id).is_none() {
            return Err(Error::NoEntry);
        }
        self.write(&values)
    }
}

#[cfg(test)]
#[path = "../../test/credential_store.rs"]
mod tests;
