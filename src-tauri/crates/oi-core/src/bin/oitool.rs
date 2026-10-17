//! `oitool` – small host-side helper used by the build scripts and by diagnostics.
//!
//! Subcommands:
//!
//! * `mkiso`        – build the boot-time seed disk or a cloud-init `cidata` image
//! * `sha256`       – hash a file (used to pin image and installer hashes)
//! * `keygen`       – create a fresh set of per-machine SSH identities
//! * `seed`         – write a complete seed disk from a JSON description
//! * `inspect`      – print what an existing data directory contains
//! * `image-info`   – print the qcow2 chain of an image

use std::path::{Path, PathBuf};

use oi_core::error::{OiError, OiResult};
use oi_core::iso::{build_iso, IsoFile, IsoOptions};
use oi_core::storage::config::{AppConfig, EnvironmentJson};
use oi_core::storage::keys::SshIdentities;
use oi_core::storage::layout::DataDir;
use oi_core::vm::seed::{SeedSpec, SEED_SCHEMA};

fn usage() -> String {
    r#"oitool <command> [options]

  mkiso --out FILE --volume ID [--joliet] name=path [name=path ...]
        Build an ISO9660 image. Names are the ISO identifiers, contents are read from disk.

  sha256 FILE [FILE ...]
        Print the SHA-256 of each file.

  keygen --dir DIR
        Generate the per-machine SSH identities into DIR.

  seed --out FILE --client-pub FILE --guest-host-private FILE --guest-client-private FILE
       --sftp-host-pub FILE [--sftp-port N] [--tunnel-port N] [--transport direct|reverse_tunnel]
        Write a boot-time seed configuration disk.

  inspect --data-dir DIR
        Report what an existing Linux data directory contains.

  image-info --qemu-img PATH --image FILE
        Print the qcow2 backing chain as JSON.
"#
    .to_string()
}

struct Args {
    positional: Vec<String>,
    flags: std::collections::BTreeMap<String, String>,
    switches: std::collections::BTreeSet<String>,
}

impl Args {
    fn parse(argv: &[String]) -> OiResult<Args> {
        let mut positional = Vec::new();
        let mut flags = std::collections::BTreeMap::new();
        let mut switches = std::collections::BTreeSet::new();
        let mut i = 0;
        while i < argv.len() {
            let a = &argv[i];
            if let Some(name) = a.strip_prefix("--") {
                if i + 1 < argv.len() && !argv[i + 1].starts_with("--") && !argv[i + 1].contains('=')
                {
                    flags.insert(name.to_string(), argv[i + 1].clone());
                    i += 2;
                } else {
                    switches.insert(name.to_string());
                    i += 1;
                }
            } else {
                positional.push(a.clone());
                i += 1;
            }
        }
        Ok(Args {
            positional,
            flags,
            switches,
        })
    }

    fn flag(&self, name: &str) -> OiResult<&str> {
        self.flags
            .get(name)
            .map(|s| s.as_str())
            .ok_or_else(|| OiError::other(format!("missing required option --{name}")))
    }

    fn opt(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(|s| s.as_str())
    }
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&argv) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("oitool: {e}");
            1
        }
    };
    std::process::exit(code);
}

fn run(argv: &[String]) -> OiResult<()> {
    let Some(command) = argv.first() else {
        println!("{}", usage());
        return Ok(());
    };
    let args = Args::parse(&argv[1..])?;
    match command.as_str() {
        "mkiso" => cmd_mkiso(&args),
        "sha256" => cmd_sha256(&args),
        "keygen" => cmd_keygen(&args),
        "seed" => cmd_seed(&args),
        "inspect" => cmd_inspect(&args),
        "image-info" => cmd_image_info(&args),
        "help" | "--help" | "-h" => {
            println!("{}", usage());
            Ok(())
        }
        other => Err(OiError::other(format!("unknown command {other}\n\n{}", usage()))),
    }
}

fn cmd_mkiso(args: &Args) -> OiResult<()> {
    let out = PathBuf::from(args.flag("out")?);
    let volume = args.flag("volume")?;
    if args.positional.is_empty() {
        return Err(OiError::other("mkiso needs at least one name=path pair"));
    }
    let mut files = Vec::new();
    for spec in &args.positional {
        let (name, path) = spec
            .split_once('=')
            .ok_or_else(|| OiError::other(format!("expected name=path, got {spec}")))?;
        let data = std::fs::read(path).map_err(|e| OiError::io(path, e))?;
        files.push(IsoFile::new(name, data));
    }
    let opts = IsoOptions::new(volume).with_joliet(args.switches.contains("joliet"));
    let image = build_iso(opts, &files)?;
    std::fs::write(&out, &image).map_err(|e| OiError::io(&out, e))?;
    println!("{} ({} bytes, {} files)", out.display(), image.len(), files.len());
    Ok(())
}

fn cmd_sha256(args: &Args) -> OiResult<()> {
    if args.positional.is_empty() {
        return Err(OiError::other("sha256 needs at least one file"));
    }
    for p in &args.positional {
        println!("{}  {}", oi_core::storage::sha256_file(Path::new(p))?, p);
    }
    Ok(())
}

fn cmd_keygen(args: &Args) -> OiResult<()> {
    let dir = args.flag("dir")?;
    let ids = SshIdentities::new(dir);
    let material = ids.generate_all()?;
    println!("client public key:     {}", material.client_public_openssh);
    println!("guest host public key: {}", material.guest_host_public_openssh);
    println!("sftp host public key:  {}", material.sftp_host_public_openssh);
    Ok(())
}

fn cmd_seed(args: &Args) -> OiResult<()> {
    let out = PathBuf::from(args.flag("out")?);
    let client_public_key = read_trimmed(args.flag("client-pub")?)?;
    let guest_host_private_key = read_trimmed(args.flag("guest-host-private")?)?;
    let guest_client_private_key = read_trimmed(args.flag("guest-client-private")?)?;
    let sftp_host_public_key = read_trimmed(args.flag("sftp-host-pub")?)?;
    let sftp_port: u16 = args
        .opt("sftp-port")
        .map(|s| s.parse())
        .transpose()
        .map_err(|e| OiError::other(format!("invalid --sftp-port: {e}")))?
        .unwrap_or(55000);
    let tunnel_port: u16 = args
        .opt("tunnel-port")
        .map(|s| s.parse())
        .transpose()
        .map_err(|e| OiError::other(format!("invalid --tunnel-port: {e}")))?
        .unwrap_or(55001);
    let transport = args.opt("transport").unwrap_or("reverse_tunnel").to_string();
    let spec = SeedSpec {
        schema: SEED_SCHEMA,
        generation: 1,
        hostname: "oi-linux".into(),
        guest_user: oi_core::GUEST_USER.into(),
        guest_workspace: oi_core::GUEST_WORKSPACE.into(),
        share_transport: transport,
        host_sftp_address: oi_core::vm::seed::SLIRP_HOST_ALIAS.into(),
        host_sftp_port: sftp_port,
        reverse_tunnel_port: tunnel_port,
        client_public_key: client_public_key.clone(),
        sftp_host_public_key: sftp_host_public_key.clone(),
        authorized_keys: client_public_key.clone(),
        win_known_hosts: oi_core::vm::seed::win_known_hosts(
            sftp_port,
            tunnel_port,
            &sftp_host_public_key,
        ),
        guest_host_private_key,
        guest_client_private_key,
    };
    let bytes = spec.write_iso(&out)?;
    println!("{} ({} bytes)", out.display(), bytes);
    Ok(())
}

fn cmd_inspect(args: &Args) -> OiResult<()> {
    let dir = PathBuf::from(args.flag("data-dir")?);
    let data = DataDir::new(&dir);
    println!("data directory: {}", data.root().display());
    println!("initialised:    {}", data.is_initialized());
    if data.environment_json().is_file() {
        let env = EnvironmentJson::load(&data.environment_json())?;
        println!("{}", serde_json::to_string_pretty(&env)?);
        if data.base_qcow2().is_file() {
            let actual = oi_core::storage::sha256_file(&data.base_qcow2())?;
            println!(
                "base.qcow2 hash matches environment.json: {}",
                actual.eq_ignore_ascii_case(&env.base_sha256)
            );
        }
    }
    if data.config_json().is_file() {
        let cfg = AppConfig::load(&data.config_json())?;
        println!("{}", serde_json::to_string_pretty(&cfg)?);
    }
    if data.metadata_db().is_file() {
        match oi_core::share::ModeDb::open(&data.metadata_db()).and_then(|d| d.verify()) {
            Ok(()) => println!("metadata.db: ok"),
            Err(e) => println!("metadata.db: PROBLEM ({e})"),
        }
    }
    Ok(())
}

fn cmd_image_info(args: &Args) -> OiResult<()> {
    let qemu_img = PathBuf::from(args.flag("qemu-img")?);
    let image = PathBuf::from(args.flag("image")?);
    let chain = oi_core::storage::verify_overlay_chain(&qemu_img, &image)?;
    println!("{}", serde_json::to_string_pretty(&chain)?);
    Ok(())
}

fn read_trimmed(path: impl AsRef<Path>) -> OiResult<String> {
    let p = path.as_ref();
    let s = std::fs::read_to_string(p).map_err(|e| OiError::io(p, e))?;
    Ok(s.trim().to_string())
}
