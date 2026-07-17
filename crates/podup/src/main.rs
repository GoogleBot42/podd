//! `podup` — the host-side release tool for podd.
//!
//! Typical flow:
//!   podup keygen --out-dir keys/
//!   podup release --channel stable --key keys/signing.key --out-dir dist/ \
//!       --app-src build/app --app-version 0.1.0+abc123 \
//!       --mcu-frozen blobs/firmware-frozen.bbin --mcu-frozen-version 4.2
//!   podup verify --pubkey keys/signing.pub --manifest dist/manifest.json --dir dist/
//!
//! Nothing here runs on the device; the device only ever *verifies*.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pod_update::manifest::{Artifact, Component, ComponentKind};
use pod_update::{package, sign, Manifest};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "podup", version, about = "Build, sign, and verify podd update bundles")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate an Ed25519 signing keypair (keep signing.key OFFLINE).
    Keygen {
        #[arg(long, default_value = "keys")]
        out_dir: PathBuf,
    },
    /// Pack a directory into a reproducible squashfs and print its Artifact JSON.
    Pack {
        #[arg(long)]
        src: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Build + sign a release manifest (and pack the app payload).
    Release(ReleaseArgs),
    /// Verify a signed manifest (and, with --dir, its artifacts) against a pubkey.
    Verify {
        #[arg(long)]
        pubkey: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        /// Directory containing the artifacts, to also check digests.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(clap::Args)]
struct ReleaseArgs {
    #[arg(long, default_value = "stable")]
    channel: String,
    /// base64 signing key file (from `keygen`).
    #[arg(long)]
    key: PathBuf,
    #[arg(long, default_value = "dist")]
    out_dir: PathBuf,

    /// Directory to pack as the app (podd + UI) payload.
    #[arg(long)]
    app_src: PathBuf,
    #[arg(long)]
    app_version: String,

    /// Optional prebuilt OS image (e.g. RAUC bundle).
    #[arg(long)]
    os: Option<PathBuf>,
    #[arg(long)]
    os_version: Option<String>,

    /// Optional STM32 Frozen MCU firmware blob (.bbin).
    #[arg(long)]
    mcu_frozen: Option<PathBuf>,
    #[arg(long)]
    mcu_frozen_version: Option<String>,

    /// Optional STM32 Sensor MCU firmware blob (.bbin).
    #[arg(long)]
    mcu_sensor: Option<PathBuf>,
    #[arg(long)]
    mcu_sensor_version: Option<String>,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Keygen { out_dir } => keygen(&out_dir),
        Cmd::Pack { src, out } => {
            let art = package::pack_squashfs(&src, &out)?;
            println!("{}", serde_json::to_string_pretty(&art)?);
            Ok(())
        }
        Cmd::Release(args) => release(args),
        Cmd::Verify {
            pubkey,
            manifest,
            dir,
        } => verify(&pubkey, &manifest, dir.as_deref()),
    }
}

fn keygen(out_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let (sk, vk) = sign::generate_keypair()?;
    let key_path = out_dir.join("signing.key");
    let pub_path = out_dir.join("signing.pub");
    std::fs::write(&key_path, sign::encode_signing_key(&sk))?;
    std::fs::write(&pub_path, sign::encode_verifying_key(&vk))?;
    // Best-effort tighten perms on the secret key.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
    }
    println!("key_id: {}", sign::key_id(&vk));
    println!("wrote {} (KEEP OFFLINE) and {}", key_path.display(), pub_path.display());
    Ok(())
}

fn load_key(path: &Path) -> Result<sign::SigningKey> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(sign::decode_signing_key(&text)?)
}

fn release(a: ReleaseArgs) -> Result<()> {
    std::fs::create_dir_all(&a.out_dir)?;
    let sk = load_key(&a.key)?;
    let mut m = Manifest::new(a.channel.clone(), now_unix());

    // Tier 2: app payload — pack the directory into a reproducible squashfs.
    let app_out = a.out_dir.join(format!("app-{}.squashfs", a.app_version));
    let app_art = package::pack_squashfs(&a.app_src, &app_out)?;
    m.components.push(Component {
        name: "podd".into(),
        kind: ComponentKind::App,
        version: a.app_version.clone(),
        artifact: app_art,
        min_app: None,
    });

    // Optional prebuilt components: copy into out_dir and record their digests.
    add_prebuilt(&mut m, &a.out_dir, "os-image", ComponentKind::Os, a.os.as_deref(), a.os_version.as_deref())?;
    add_prebuilt(&mut m, &a.out_dir, "mcu-frozen", ComponentKind::McuFrozen, a.mcu_frozen.as_deref(), a.mcu_frozen_version.as_deref())?;
    add_prebuilt(&mut m, &a.out_dir, "mcu-sensor", ComponentKind::McuSensor, a.mcu_sensor.as_deref(), a.mcu_sensor_version.as_deref())?;

    let signed = sign::sign_manifest(&m, &sk)?;
    let manifest_path = a.out_dir.join("manifest.json");
    std::fs::write(&manifest_path, signed.to_json_pretty()?)?;
    println!("wrote {} ({} component(s), key_id {})", manifest_path.display(), m.components.len(), signed.key_id);
    Ok(())
}

fn add_prebuilt(
    m: &mut Manifest,
    out_dir: &Path,
    name: &str,
    kind: ComponentKind,
    file: Option<&Path>,
    version: Option<&str>,
) -> Result<()> {
    let Some(file) = file else { return Ok(()) };
    let version = version
        .ok_or_else(|| anyhow::anyhow!("{name}: file given without a --{name}-version"))?;
    let dest = out_dir.join(file.file_name().unwrap());
    if dest != *file {
        std::fs::copy(file, &dest).with_context(|| format!("copying {}", file.display()))?;
    }
    let art: Artifact = package::artifact_for_file(&dest)?;
    m.components.push(Component {
        name: name.into(),
        kind,
        version: version.to_string(),
        artifact: art,
        min_app: None,
    });
    Ok(())
}

fn verify(pubkey: &Path, manifest: &Path, dir: Option<&Path>) -> Result<()> {
    let vk = sign::decode_verifying_key(&std::fs::read_to_string(pubkey)?)?;
    let signed = sign::SignedManifest::from_json(&std::fs::read_to_string(manifest)?)?;
    let m = sign::verify_manifest(&signed, &[vk])?;
    println!("signature OK (key_id {}, channel {}, {} component(s))", signed.key_id, m.channel, m.components.len());
    if let Some(dir) = dir {
        for c in &m.components {
            m.verify_artifact(c, &dir.join(&c.artifact.filename))?;
            println!("  artifact OK: {} ({:?}) {}", c.name, c.kind, c.artifact.filename);
        }
    }
    Ok(())
}
