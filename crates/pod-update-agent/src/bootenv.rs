//! U-Boot environment access for the OS A/B state machine.
//!
//! The env vars form the contract with `uboot-env.txt`'s bootcmd script (and
//! `install/podd-slot-install.sh`, which uses the same scheme): `mmcpart` /
//! `next_mmcpart` select the slot (1=A, 2=B), and `upgrade_available` /
//! `bootcount` / `ustate` carry the trial state. See the state-machine comment
//! block in `os/board/eightsleep/imx8mm-varsom/uboot-env.txt` — that file owns
//! the semantics.
//!
//! Behind a trait so agent logic tests never need `fw_printenv` or a real env
//! partition.

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::io::Write;
use std::process::Command;

/// Read/write the U-Boot environment.
///
/// `set_batch` MUST apply all vars in a single env rewrite (one CRC update):
/// the arm/disarm var sets are only consistent together, and each separate
/// rewrite is a power-loss corruption window on the single-copy env.
pub trait BootEnv: Send + Sync {
    /// Value of `key`, `None` if unset. `Err` means the env itself is
    /// unreadable (no `fw_env.config`, no tools, not on-device) — callers use
    /// this to distinguish "not an A/B system" from "var unset".
    fn get(&self, key: &str) -> Result<Option<String>>;
    /// Set all `vars` in one atomic-as-the-hardware-allows env rewrite.
    fn set_batch(&self, vars: &[(&str, &str)]) -> Result<()>;
}

/// Real implementation shelling out to libubootenv's `fw_printenv` /
/// `fw_setenv` (configured by `/etc/fw_env.config`; the tools take their own
/// lock file, so concurrent invocations serialize).
pub struct FwEnv;

impl BootEnv for FwEnv {
    fn get(&self, key: &str) -> Result<Option<String>> {
        // Dump the whole env (4 KiB) rather than querying one var: a nonzero
        // exit then cleanly means "env unreadable", never "var unset".
        let out = Command::new("fw_printenv").output()?;
        if !out.status.success() {
            return Err(Error::Config(format!(
                "fw_printenv failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let dump = String::from_utf8_lossy(&out.stdout);
        for line in dump.lines() {
            if let Some((k, v)) = line.split_once('=') {
                if k == key {
                    return Ok(Some(v.to_string()));
                }
            }
        }
        Ok(None)
    }

    fn set_batch(&self, vars: &[(&str, &str)]) -> Result<()> {
        // libubootenv's `fw_setenv -s <file>`: one `name=value` per line,
        // applied as a single load-modify-store of the env (one CRC rewrite).
        let path = std::env::temp_dir().join(format!(
            "podd-fw-setenv-{}-{:x}.txt",
            std::process::id(),
            vars.as_ptr() as usize
        ));
        {
            let mut script = std::fs::File::create(&path)?;
            for (k, v) in vars {
                writeln!(script, "{k}={v}")?;
            }
            script.flush()?;
        }
        let out = Command::new("fw_setenv").arg("-s").arg(&path).output();
        let _ = std::fs::remove_file(&path);
        let out = out?;
        if !out.status.success() {
            return Err(Error::Config(format!(
                "fw_setenv -s failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }
}

/// In-memory test double; records every `set_batch` call.
#[derive(Default)]
pub struct FakeEnv {
    pub vars: std::sync::Mutex<HashMap<String, String>>,
    pub batches: std::sync::Mutex<Vec<Vec<(String, String)>>>,
}

impl FakeEnv {
    pub fn with(vars: &[(&str, &str)]) -> Self {
        let env = FakeEnv::default();
        env.vars.lock().unwrap().extend(
            vars.iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        env
    }
}

impl BootEnv for FakeEnv {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.vars.lock().unwrap().get(key).cloned())
    }

    fn set_batch(&self, vars: &[(&str, &str)]) -> Result<()> {
        let mut map = self.vars.lock().unwrap();
        for (k, v) in vars {
            map.insert(k.to_string(), v.to_string());
        }
        self.batches.lock().unwrap().push(
            vars.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        );
        Ok(())
    }
}

/// A [`BootEnv`] whose reads/writes always fail — models a dev box with no
/// `fw_env.config` (used in tests for the graceful-degradation paths).
pub struct UnreadableEnv;

impl BootEnv for UnreadableEnv {
    fn get(&self, _key: &str) -> Result<Option<String>> {
        Err(Error::Config("boot env unreadable (test)".into()))
    }
    fn set_batch(&self, _vars: &[(&str, &str)]) -> Result<()> {
        Err(Error::Config("boot env unreadable (test)".into()))
    }
}
