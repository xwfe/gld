//! 临时状态只属于本次协议预检；不读取已有 Cua 配置或接管常驻 App daemon。

use super::*;
use std::collections::BTreeMap;
use std::fs;

pub(super) struct Directory {
    pub path: PathBuf,
    cleanup_attempted: std::cell::Cell<bool>,
}

impl Directory {
    pub fn create(parent: &Path) -> Result<Self> {
        let parent = parent
            .canonicalize()
            .map_err(|_| fail("PROBE_STATE", "Temporary parent is unavailable"))?;
        let path = parent.join(format!(
            "gld-computer-probe-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|_| fail("PROBE_STATE", "Cannot create private probe directory"))?;
        let state = Self {
            path,
            cleanup_attempted: std::cell::Cell::new(false),
        };
        for name in ["config", "local", "data", "state", "cache", "driver", "tmp"] {
            builder
                .create(state.path.join(name))
                .map_err(|_| fail("PROBE_STATE", "Cannot prepare private provider state"))?;
        }
        Ok(state)
    }

    pub fn environment(&self) -> Result<BTreeMap<String, String>> {
        let mut env = BTreeMap::new();
        for key in ["SystemRoot", "WINDIR", "COMSPEC", "PATHEXT"] {
            if let Ok(value) = std::env::var(key) {
                env.insert(key.into(), value);
            }
        }
        for (key, suffix) in [
            ("HOME", ""),
            ("USERPROFILE", ""),
            ("APPDATA", "config"),
            ("LOCALAPPDATA", "local"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("CUA_DRIVER_HOME", "driver"),
            ("CUA_DRIVER_RS_HOME", "driver"),
            ("TMPDIR", "tmp"),
            ("TMP", "tmp"),
            ("TEMP", "tmp"),
        ] {
            let path = if suffix.is_empty() {
                self.path.clone()
            } else {
                self.path.join(suffix)
            };
            env.insert(
                key.into(),
                path.to_str()
                    .ok_or_else(|| fail("PROBE_STATE", "Probe directory must be UTF-8"))?
                    .into(),
            );
        }
        env.insert("CUA_DRIVER_PERMISSION_MODE".into(), "standard".into());
        env.insert("CUA_DRIVER_RS_TELEMETRY_ENABLED".into(), "false".into());
        env.insert("NO_COLOR".into(), "1".into());
        Ok(env)
    }

    pub fn remove(&self) -> Result<()> {
        self.cleanup_attempted.set(true);
        match fs::symlink_metadata(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(meta)
                if meta.is_dir()
                    && !meta.file_type().is_symlink()
                    && self.path.canonicalize().ok().as_ref() == Some(&self.path) => {}
            _ => {
                return Err(fail(
                    "PROBE_CLEANUP_FAILED",
                    "Probe directory was redirected or is unavailable",
                ))
            }
        }
        fs::remove_dir_all(&self.path)
            .map_err(|_| fail("PROBE_CLEANUP_FAILED", "Probe state could not be removed"))
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if !self.cleanup_attempted.get() {
            let _ = self.remove();
        }
    }
}
