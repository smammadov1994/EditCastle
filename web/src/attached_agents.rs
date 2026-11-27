use crate::editor::private_write;
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};
use subtle::ConstantTimeEq;

pub struct Access {
    key: Option<String>,
    key_file: PathBuf,
    preference_file: PathBuf,
    enabled: bool,
    revision: u64,
}

#[derive(Serialize)]
pub struct Settings {
    configured: bool,
    enabled: bool,
}

impl Access {
    pub fn disabled(root: &Path) -> Self {
        Self {
            key: None,
            key_file: root.join("mcp-key"),
            preference_file: root.join("mcp-enabled"),
            enabled: false,
            revision: 0,
        }
    }

    pub fn open(root: &Path, override_path: Option<PathBuf>, owner: Option<&str>) -> Result<Self> {
        let mut access = Self::disabled(root);
        let managed = override_path.is_none();
        if let Some(path) = override_path {
            access.key_file = path;
        }
        access.key = match fs::read_to_string(&access.key_file) {
            Ok(key) => {
                let key = key.trim().to_owned();
                ensure!(
                    key.len() >= 32 && key.bytes().all(|byte| byte.is_ascii_graphic()),
                    "The MCP token needs at least 32 printable characters."
                );
                ensure!(
                    Some(key.as_str()) != owner,
                    "Use a different token for MCP and studio sign-in."
                );
                Some(key)
            }
            Err(error) if managed && error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        access.enabled = match fs::read_to_string(&access.preference_file) {
            Ok(value) => value.trim() == "true",
            Err(error) if error.kind() == ErrorKind::NotFound => true,
            Err(_) => false,
        } && access.key.is_some();
        Ok(access)
    }

    pub fn settings(&self) -> Settings {
        Settings {
            configured: self.key.is_some(),
            enabled: self.enabled,
        }
    }

    pub fn set_enabled(&mut self, enabled: bool, generate: impl FnOnce() -> String) -> Result<()> {
        if enabled == self.enabled {
            return Ok(());
        }
        if enabled && self.key.is_none() {
            // Keep startup disabled if provisioning stops after writing the key.
            private_write(&self.preference_file, b"false")?;
            let key = generate();
            private_write(&self.key_file, key.as_bytes())?;
            self.key = Some(key);
        }
        private_write(
            &self.preference_file,
            if enabled { b"true" } else { b"false" },
        )?;
        self.enabled = enabled;
        self.revision += 1;
        Ok(())
    }

    pub fn key(&self) -> Option<&str> {
        self.key.as_deref().filter(|_| self.enabled)
    }

    pub fn key_file(&self) -> &Path {
        &self.key_file
    }

    pub fn authorize(&self, received: &str) -> Option<u64> {
        let key = self.key()?;
        bool::from(key.as_bytes().ct_eq(received.as_bytes())).then_some(self.revision)
    }

    pub fn accepts(&self, revision: u64) -> bool {
        self.enabled && self.revision == revision
    }
}
